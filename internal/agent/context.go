package agent

import (
	"cmp"
	"encoding/json/jsontext"
	"html"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
)

const prunedOutput = "[output pruned to fit the context window]"

type transcript struct {
	host        *Host
	thread      *Thread
	pruned      map[string]bool
	names       map[string]string
	order       map[string]int
	messages    []model.Message
	outstanding map[string]bool
	results     []model.ToolResult
	deferred    []userTurn
	followups   []userTurn
	asides      []string
	held        []event.Event
	toolFree    bool
	err         error
}

type userTurn struct {
	text   string
	images []model.Image
}

func (t *Thread) messages(v view) ([]model.Message, error) {
	path := event.ActivePath(v.records)
	kept, summary := afterCompaction(path)
	b := &transcript{
		host: t.host, thread: t, pruned: prunedCalls(path), names: make(map[string]string), order: make(map[string]int),
		outstanding: make(map[string]bool),
	}
	if summary != "" {
		b.messages = append(b.messages, model.Message{Role: "user", Text: summary})
	}
	late := lateInputs(kept)
	for i, item := range kept {
		if late[i] {
			b.held = append(b.held, item.Event)
			continue
		}
		b.add(item.Event)
		if _, ok := item.Event.(event.AssistantMessage); ok {
			b.release()
		}
	}
	b.flushResults()
	return b.messages, b.err
}

func needsModel(messages []model.Message) bool {
	return len(messages) > 0 && messages[len(messages)-1].Role == "user"
}

func afterCompaction(path []event.Seqd) ([]event.Seqd, string) {
	for i := len(path) - 1; i >= 0; i-- {
		compaction, ok := path[i].Event.(event.Compaction)
		if !ok {
			continue
		}
		kept := make([]event.Seqd, 0, len(path))
		for _, item := range path {
			if _, marker := item.Event.(event.Compaction); !marker && item.Seq >= compaction.FirstKeptSeq {
				kept = append(kept, item)
			}
		}
		return kept, compaction.Summary
	}
	return path, ""
}

func prunedCalls(path []event.Seqd) map[string]bool {
	pruned := make(map[string]bool)
	for _, item := range path {
		if value, ok := item.Event.(event.ContextPruned); ok {
			for _, id := range value.CallIDs {
				pruned[id] = true
			}
		}
	}
	return pruned
}

func lateInputs(path []event.Seqd) []bool {
	late := make([]bool, len(path))
	var contextSeq uint64
	for i := len(path) - 1; i >= 0; i-- {
		switch value := path[i].Event.(type) {
		case event.AssistantMessage:
			contextSeq = value.ContextSeq
		case event.TurnStarted, event.TurnCompleted:
			contextSeq = 0
		case event.UserMessage, event.AdvisorNote:
			late[i] = contextSeq != 0 && path[i].Seq > contextSeq
		}
	}
	return late
}

func (b *transcript) release() {
	held := b.held
	b.held = nil
	for _, item := range held {
		b.add(item)
	}
}

func (b *transcript) add(e event.Event) {
	switch value := e.(type) {
	case event.TurnStarted:
		b.toolFree = false
	case event.TurnCompleted:
		b.emitFollowups()
	case event.UserMessage:
		b.user(value)
	case event.AssistantMessage:
		b.assistant(value)
	case event.ToolCompleted:
		b.result(value)
	case event.AdvisorNote:
		b.note(value)
	case event.ToolJudged:
		b.judged(value)
	case event.TurnJudged:
		b.turnJudged(value)
	}
}

func (b *transcript) judged(e event.ToolJudged) {
	if e.Feedback {
		b.emitUser(userTurn{text: reviewFeedback(e)})
	}
}

func (b *transcript) turnJudged(e event.TurnJudged) {
	if e.Feedback {
		b.emitUser(userTurn{text: reviewTurnFeedback(e)})
	}
}

func (b *transcript) user(e event.UserMessage) {
	turn := userTurn{text: b.renderUser(e), images: b.images(e.Images)}
	if e.Kind == string(InputFollowup) && !b.toolFree {
		b.followups = append(b.followups, turn)
		return
	}
	b.emitUser(turn)
}

func (b *transcript) images(images []event.Image) []model.Image {
	out, err := b.thread.imageParts(images)
	b.keep(err)
	return out
}

func (b *transcript) renderUser(e event.UserMessage) string {
	text, err := b.host.loadText(e.Text, e.TextBlob)
	b.keep(err)
	var out strings.Builder
	out.WriteString(text)
	for _, mention := range e.Mentions {
		body, err := b.host.loadText(mention.Text, mention.TextBlob)
		b.keep(err)
		out.WriteString("\n\n<file path=\"" + html.EscapeString(mention.Path) + "\">\n" + body + "\n</file>")
	}
	for _, notice := range e.Notices {
		body, err := b.host.loadText(notice.Text, notice.TextBlob)
		b.keep(err)
		out.WriteString("\n\n<system-notice>\n" + strings.TrimSpace(body) + "\n</system-notice>")
	}
	for _, aside := range b.asides {
		out.WriteString("\n\n" + aside)
	}
	b.asides = nil
	return out.String()
}

func (b *transcript) note(e event.AdvisorNote) {
	if e.Delivery == string(DeliveryAside) {
		b.asides = append(b.asides, e.Render())
		return
	}
	b.emitUser(userTurn{text: e.Render()})
}

func (b *transcript) emitUser(turn userTurn) {
	if len(b.outstanding) > 0 {
		b.deferred = append(b.deferred, turn)
		return
	}
	b.messages = append(b.messages, model.Message{Role: "user", Text: turn.text, Images: turn.images})
}

func (b *transcript) emitFollowups() {
	followups := b.followups
	b.followups = nil
	for _, turn := range followups {
		b.emitUser(turn)
	}
}

func (b *transcript) assistant(e event.AssistantMessage) {
	b.flushResults()
	text, err := b.host.loadText(e.Text, e.TextBlob)
	b.keep(err)
	reasoning, err := b.host.loadText(e.Reasoning, e.ReasoningBlob)
	b.keep(err)
	message := model.Message{Role: "assistant", Text: text, Reasoning: reasoning, ToolCalls: make([]model.ToolCall, 0, len(e.ToolCalls))}
	for index, call := range e.ToolCalls {
		b.order[call.ID] = index
		args, err := b.host.loadRaw(call.Args, call.ArgsBlob)
		b.keep(err)
		message.ToolCalls = append(message.ToolCalls, model.ToolCall{
			ID: call.ID, Name: call.Name, Args: args, Freeform: call.Freeform, Resolved: call.Resolved,
			Signature: call.Signature, RedactedThinking: call.RedactedThinking,
		})
		b.outstanding[call.ID] = true
		b.names[call.ID] = call.Name
	}
	b.messages = append(b.messages, message)
	b.toolFree = len(e.ToolCalls) == 0
	if b.toolFree {
		b.emitFollowups()
	}
}

func (b *transcript) result(e event.ToolCompleted) {
	output := e.Output
	if b.pruned[e.CallID] {
		output = prunedOutput
	}
	b.results = append(b.results, model.ToolResult{ID: e.CallID, Name: b.names[e.CallID], Output: jsonString(output), IsError: !e.OK})
	delete(b.outstanding, e.CallID)
	if len(b.outstanding) == 0 {
		b.flushResults()
	}
}

func (b *transcript) flushResults() {
	if len(b.results) > 0 {
		slices.SortStableFunc(b.results, func(left, right model.ToolResult) int {
			return cmp.Compare(b.order[left.ID], b.order[right.ID])
		})
		b.messages = append(b.messages, model.Message{Role: "user", ToolResults: b.results})
		b.results = nil
	}
	clear(b.outstanding)
	deferred := b.deferred
	b.deferred = nil
	for _, turn := range deferred {
		b.emitUser(turn)
	}
}

func (b *transcript) keep(err error) {
	if err != nil && b.err == nil {
		b.err = err
	}
}

func jsonString(text string) jsontext.Value {
	if !utf8.ValidString(text) {
		text = strings.ToValidUTF8(text, "\uFFFD")
	}
	quoted, _ := jsontext.AppendQuote(nil, text)
	return jsontext.Value(quoted)
}
