package rpc

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"log/slog"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/textdiff"
)

const (
	reasoningSuffix     = "-reasoning"
	methodTurnStarted   = "turn/started"
	methodTurnCompleted = "turn/completed"
	methodItemStarted   = "item/started"
	methodItemCompleted = "item/completed"
)

type sink interface {
	turn(method string, turn Turn)
	item(method, turnID string, it *Item)
	notify(method string, build func() writer)
}

type tracker struct {
	threadID  string
	created   event.SessionCreated
	load      func(*event.BlobRef) ([]byte, error)
	childCwd  func(id string) string
	keywords  func(threadID, text string, kind agent.InputKind) []agent.KeywordMatch
	log       *slog.Logger
	sink      sink
	turn      string
	lastTurn  string
	turnAt    time.Time
	items     map[string]*Item
	calls     map[string]event.ToolCall
	classes   map[string]string
	approvals map[string]string
	declined  map[string]bool
	callAt    map[string]time.Time
	usage     ThreadTokenUsage
	seq       uint64
	statusSeq uint64
}

func newTracker(threadID string, load func(*event.BlobRef) ([]byte, error), childCwd func(string) string, keywords func(threadID, text string, kind agent.InputKind) []agent.KeywordMatch, log *slog.Logger) *tracker {
	return &tracker{
		threadID: threadID, load: load, childCwd: childCwd, keywords: keywords, log: log,
		items: map[string]*Item{}, calls: map[string]event.ToolCall{}, classes: map[string]string{},
		approvals: map[string]string{}, declined: map[string]bool{},
		callAt: map[string]time.Time{},
	}
}

type eventHandler func(t *tracker, seq uint64, ev event.Event)

var eventHandlers = map[string]eventHandler{
	event.TypeSessionCreated:    func(t *tracker, _ uint64, ev event.Event) { t.created, _ = ev.(event.SessionCreated) },
	event.TypeTurnStarted:       (*tracker).turnStarted,
	event.TypeTurnCompleted:     (*tracker).turnCompleted,
	event.TypeUserMessage:       (*tracker).userMessage,
	event.TypeAssistantMessage:  (*tracker).assistantMessage,
	event.TypeToolStarted:       (*tracker).toolStarted,
	event.TypeToolCompleted:     (*tracker).toolCompleted,
	event.TypeApprovalRequested: (*tracker).approvalRequested,
	event.TypeApprovalResolved:  (*tracker).approvalResolved,
	event.TypeCompaction:        (*tracker).compaction,
	event.TypeModelRetry:        (*tracker).modelRetry,
	event.TypeChildSpawned:      (*tracker).childSpawned,
	event.TypeAdvisorNote:       (*tracker).advisorNote,
	event.TypeThreadNamed:       (*tracker).threadNamed,
	event.TypeThreadArchived:    (*tracker).threadArchived,
	event.TypeBranch:            (*tracker).branch,
	event.TypeToolJudged:        (*tracker).toolJudged,
	event.TypeTurnJudged:        (*tracker).turnJudged,
	event.TypeSandboxChosen:     (*tracker).sandboxChosen,
}

func (t *tracker) apply(seq uint64, ev event.Event) {
	t.seq = seq
	if handle, ok := eventHandlers[ev.Type()]; ok {
		handle(t, seq, ev)
	}
}

func (t *tracker) currentTurn() string { return cmp.Or(t.turn, t.lastTurn) }

func (t *tracker) text(text string, blob *event.BlobRef) string {
	if blob == nil {
		return text
	}
	data, err := t.load(blob)
	if err != nil {
		t.log.Warn("rpc: load blob", "thread", t.threadID, "blob", blob.Blob, "error", err)
		return text
	}
	return string(data)
}

func (t *tracker) args(args jsontext.Value, blob *event.BlobRef) jsontext.Value {
	if blob == nil {
		return args
	}
	return jsontext.Value(t.text(string(args), blob))
}

func (t *tracker) setStatus(kind string, flags ...string) {
	seq := t.seq
	if seq <= t.statusSeq {
		seq = t.statusSeq + 1
	}
	t.statusSeq = seq
	status := ThreadStatus{Type: kind, ActiveFlags: flags}
	t.sink.notify("thread/status/changed", func() writer {
		return ThreadStatusChangedNotification{ThreadID: t.threadID, Status: status, Seq: seq}
	})
}

func (t *tracker) startComplete(turnID string, it *Item) {
	t.sink.item(methodItemStarted, turnID, it)
	t.sink.item(methodItemCompleted, turnID, it)
}

func (t *tracker) turnStarted(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnStarted)
	t.turn, t.lastTurn, t.turnAt = e.TurnID, e.TurnID, e.StartedAt
	t.sink.turn(methodTurnStarted, Turn{ID: e.TurnID, Status: statusInProgress, StartedAt: unixSeconds(e.StartedAt)})
	t.setStatus("active")
}

func (t *tracker) turnCompleted(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnCompleted)
	turn := Turn{ID: e.TurnID, Status: e.Status, StartedAt: unixSeconds(t.turnAt), CompletedAt: unixSeconds(e.CompletedAt), DurationMs: durationMs(t.turnAt, e.CompletedAt)}
	if e.Error != "" {
		turn.Error = &TurnError{Message: e.Error}
		t.sink.notify("error", func() writer {
			return ErrorNotification{ThreadID: t.threadID, TurnID: e.TurnID, Error: TurnError{Message: e.Error}}
		})
	}
	t.turn = ""
	t.turnAt = time.Time{}
	clear(t.items)
	t.sink.turn(methodTurnCompleted, turn)
	t.setStatus("idle")
}

func (t *tracker) userMessage(_ uint64, ev event.Event) {
	e, _ := ev.(event.UserMessage)
	text := t.text(e.Text, e.TextBlob)
	item := &Item{Type: kindUser, ID: e.ItemID, Text: text, Keywords: t.noticeMatches(e, text), Images: itemImages(e.Images)}
	t.startComplete(e.TurnID, item)
}

func (t *tracker) noticeMatches(e event.UserMessage, text string) []KeywordMatch {
	kind := agent.InputKind(cmp.Or(e.Kind, string(agent.InputPrompt)))
	journaled := make(map[string]bool, len(e.Notices))
	for _, notice := range e.Notices {
		journaled[notice.Word] = true
	}
	var out []KeywordMatch
	for _, match := range t.keywords(t.threadID, text, kind) {
		if journaled[match.Word] {
			out = append(out, keywordMatchOf(match))
		}
	}
	return out
}

func (t *tracker) assistantMessage(_ uint64, ev event.Event) {
	e, _ := ev.(event.AssistantMessage)
	t.finishStream(e.TurnID, kindReasoning, e.ItemID+reasoningSuffix, t.text(e.Reasoning, e.ReasoningBlob))
	t.finishStream(e.TurnID, kindAgent, e.ItemID, t.text(e.Text, e.TextBlob))
	for _, call := range e.ToolCalls {
		call.Args = t.args(call.Args, call.ArgsBlob)
		t.calls[call.ID] = call
	}
	if e.ContextWindow > 0 {
		t.usage.ModelContextWindow = e.ContextWindow
	}
	if e.Usage == (event.Usage{}) {
		return
	}
	last := TokenUsageBreakdown{
		InputTokens: e.Usage.Input, CachedInputTokens: e.Usage.CacheRead, CacheWriteInputTokens: e.Usage.CacheWrite,
		OutputTokens: e.Usage.Output, TotalTokens: e.Usage.Input + e.Usage.Output,
	}
	t.usage.Last = last
	t.usage.Total = t.usage.Total.add(last)
	usage := t.usage
	t.sink.notify("thread/tokenUsage/updated", func() writer {
		return ThreadTokenUsageUpdatedNotification{ThreadID: t.threadID, TurnID: e.TurnID, TokenUsage: usage}
	})
}

func (t *tracker) usageSnapshot() *ThreadTokenUsage {
	if t.usage.Last == (TokenUsageBreakdown{}) && t.usage.ModelContextWindow == 0 {
		return nil
	}
	usage := t.usage
	return &usage
}

func (t *tracker) finishStream(turnID, kind, id, text string) {
	it, open := t.items[id]
	if !open {
		if text == "" {
			return
		}
		it = &Item{Type: kind, ID: id}
		t.sink.item(methodItemStarted, turnID, it)
	}
	delete(t.items, id)
	it.Text = text
	t.sink.item(methodItemCompleted, turnID, it)
}

func (t *tracker) stream(kind, id string) {
	if _, ok := t.items[id]; ok {
		return
	}
	it := &Item{Type: kind, ID: id}
	t.items[id] = it
	t.sink.item(methodItemStarted, t.currentTurn(), it)
}

func (t *tracker) agentDelta(itemID, delta string) {
	t.stream(kindAgent, itemID)
	turnID := t.currentTurn()
	t.sink.notify("item/agentMessage/delta", func() writer {
		return AgentMessageDeltaNotification{ThreadID: t.threadID, TurnID: turnID, ItemID: itemID, Delta: delta}
	})
}

func (t *tracker) reasoningDelta(itemID, delta string) {
	id := itemID + reasoningSuffix
	t.stream(kindReasoning, id)
	turnID := t.currentTurn()
	t.sink.notify("item/reasoning/textDelta", func() writer {
		return ReasoningTextDeltaNotification{ThreadID: t.threadID, TurnID: turnID, ItemID: id, Delta: delta}
	})
}

func (t *tracker) execOutput(callID, delta string) {
	it, open := t.items[callID]
	if !open || it.Type != kindCommand {
		return
	}
	turnID := t.currentTurn()
	t.sink.notify("item/commandExecution/outputDelta", func() writer {
		return CommandExecutionOutputDeltaNotification{ThreadID: t.threadID, TurnID: turnID, ItemID: callID, Delta: delta}
	})
}

func (t *tracker) retryWait(turnID string, wait time.Duration) {
	t.sink.notify("dalgona/retry/wait", func() writer {
		return RetryWaitNotification{ThreadID: t.threadID, TurnID: turnID, RemainingMS: wait.Milliseconds()}
	})
}

func (t *tracker) sandboxChosen(_ uint64, ev event.Event) {
	e, _ := ev.(event.SandboxChosen)
	choice := "accept"
	if e.Choice == event.SandboxChoiceDangerous {
		choice = "dangerouslyAccept"
	}
	t.sink.notify("dalgona/sandbox/chosen", func() writer {
		return SandboxChosenNotification{ThreadID: t.threadID, Cause: e.Cause, Choice: choice}
	})
}

func (t *tracker) toolItem(callID, tool, class string, args jsontext.Value, files []event.FileChange) *Item {
	it := &Item{Type: toolKind(tool, class), ID: callID, Tool: tool, Arguments: args, Status: statusInProgress}
	switch it.Type {
	case kindCommand:
		it.Command = argString(args, "command")
		it.Cwd = cmp.Or(argString(args, "cwd"), t.created.Cwd)
	case kindFileChange:
		it.Changes = t.fileChanges(files)
	case kindMCP:
		it.Server, it.Tool, _ = strings.Cut(tool, "__")
	}
	return it
}

func (t *tracker) fileChanges(files []event.FileChange) []FileChange {
	changes := make([]FileChange, 0, len(files))
	for _, file := range files {
		kind := kindUpdate
		switch {
		case file.PreSHA == "":
			kind = kindAdd
		case file.PostSHA == "":
			kind = kindDelete
		}
		changes = append(changes, t.fileChange(file, kind))
	}
	return changes
}

func (t *tracker) fileChange(file event.FileChange, kind string) FileChange {
	pre, preOK := t.image(file.Path, file.PreSHA, file.PreBlob)
	post, postOK := t.image(file.Path, file.PostSHA, file.PostBlob)
	if !preOK || !postOK || len(pre) > textdiff.Limit || len(post) > textdiff.Limit {
		return omittedChange(file.Path, kind)
	}
	return diffChange(file.Path, kind, pre, post)
}

func (t *tracker) image(path, sha string, blob *event.BlobRef) ([]byte, bool) {
	if sha == "" {
		return nil, true
	}
	if blob == nil {
		return nil, false
	}
	data, err := t.load(blob)
	if err != nil {
		t.log.Warn("file change image unreadable", "path", path, "error", err)
		return nil, false
	}
	return data, true
}

func (t *tracker) openCall(callID string) *Item {
	if it, ok := t.items[callID]; ok {
		return it
	}
	call := t.calls[callID]
	it := t.toolItem(callID, call.Name, t.classes[callID], call.Args, nil)
	t.items[callID] = it
	t.sink.item(methodItemStarted, t.currentTurn(), it)
	return it
}

func (t *tracker) toolStarted(_ uint64, ev event.Event) {
	e, _ := ev.(event.ToolStarted)
	fresh := t.toolItem(e.CallID, e.Tool, e.Class, t.args(e.Args, e.ArgsBlob), e.Files)
	if !e.StartedAt.IsZero() {
		t.callAt[e.CallID] = e.StartedAt
	}
	if it, ok := t.items[e.CallID]; ok {
		*it = *fresh
		return
	}
	t.calls[e.CallID] = event.ToolCall{ID: e.CallID, Name: e.Tool, Args: fresh.Arguments}
	t.items[e.CallID] = fresh
	t.sink.item(methodItemStarted, e.TurnID, fresh)
}

func (t *tracker) toolCompleted(_ uint64, ev event.Event) {
	e, _ := ev.(event.ToolCompleted)
	it := t.openCall(e.CallID)
	switch {
	case e.OK:
		it.Status = statusCompleted
	case t.declined[e.CallID] && (it.Type == kindCommand || it.Type == kindFileChange):
		it.Status = statusDeclined
	default:
		it.Status = statusFailed
	}
	it.Output, it.ExitCode = e.Output, e.ExitCode
	it.Spill = outputSpill(e.Spill)
	t.finishDuration(it, e.CallID, e.CompletedAt)
	turnID := t.currentTurn()
	t.sink.item(methodItemCompleted, turnID, it)
	if it.Tool == "todo" && e.OK {
		t.plan(turnID, it.Arguments)
	}
	delete(t.items, e.CallID)
	delete(t.calls, e.CallID)
	delete(t.classes, e.CallID)
	delete(t.declined, e.CallID)
}

func durationMs(from, to time.Time) int64 {
	if from.IsZero() || to.IsZero() {
		return 0
	}
	return to.Sub(from).Milliseconds()
}

func (t *tracker) finishDuration(it *Item, callID string, completed time.Time) {
	started, ok := t.callAt[callID]
	delete(t.callAt, callID)
	if !ok || completed.IsZero() {
		return
	}
	ms := completed.Sub(started).Milliseconds()
	it.DurationMs = &ms
}

func (t *tracker) plan(turnID string, args jsontext.Value) {
	steps := todoSteps(args)
	t.sink.notify("turn/plan/updated", func() writer {
		return TurnPlanUpdatedNotification{ThreadID: t.threadID, TurnID: turnID, Plan: steps}
	})
}

func todoSteps(args jsontext.Value) []TurnPlanStep {
	steps := []TurnPlanStep{}
	_ = decodeParams(args, func(name string, d *jsontext.Decoder) error {
		if name != "items" {
			return d.SkipValue()
		}
		return readArray(d, func() error {
			var step TurnPlanStep
			err := readMembers(d, func(name string, d *jsontext.Decoder) error {
				switch name {
				case "text":
					v, err := readString(d)
					step.Step = v
					return err
				case "status":
					v, err := readString(d)
					step.Status = planStatus(v)
					return err
				}
				return d.SkipValue()
			})
			steps = append(steps, step)
			return err
		})
	})
	return steps
}

func planStatus(status string) string {
	switch status {
	case "in_progress":
		return statusInProgress
	case "done":
		return statusCompleted
	}
	return "pending"
}

func (t *tracker) approvalRequested(_ uint64, ev event.Event) {
	e, _ := ev.(event.ApprovalRequested)
	t.approvals[e.RequestID] = e.CallID
	if e.Class != "" {
		t.classes[e.CallID] = e.Class
	}
	t.openCall(e.CallID)
	t.setStatus("active", "waitingOnApproval")
}

func (t *tracker) approvalResolved(_ uint64, ev event.Event) {
	e, _ := ev.(event.ApprovalResolved)
	callID := t.approvals[e.RequestID]
	delete(t.approvals, e.RequestID)
	if e.Decision != "accept" && e.Decision != "accept_session" {
		t.declined[callID] = true
	}
	t.sink.notify("serverRequest/resolved", func() writer {
		return ServerRequestResolvedNotification{ThreadID: t.threadID, RequestID: e.RequestID, Resolution: agent.ApprovalResolution(e.Decision, e.Reason)}
	})
	if t.turn != "" {
		t.setStatus("active")
	}
}

func (t *tracker) compaction(seq uint64, _ event.Event) {
	t.startComplete(t.currentTurn(), &Item{Type: kindCompaction, ID: fmt.Sprintf("compaction-%d", seq)})
}

func (t *tracker) modelRetry(_ uint64, ev event.Event) {
	e, _ := ev.(event.ModelRetry)
	message := fmt.Sprintf("model request failed (%s, status %d); retry %d in %d ms", e.Verdict, e.Status, e.Attempt, e.DelayMS)
	t.sink.notify("error", func() writer {
		return ErrorNotification{ThreadID: t.threadID, TurnID: e.TurnID, WillRetry: true, Error: TurnError{Message: message}}
	})
}

func (t *tracker) childSpawned(_ uint64, ev event.Event) {
	e, _ := ev.(event.ChildSpawned)
	t.sink.notify("dalgona/thread/childSpawned", func() writer {
		return ChildSpawnedNotification{ThreadID: t.threadID, ChildID: e.ChildID, Cwd: cmp.Or(t.childCwd(e.ChildID), t.created.Cwd)}
	})
}

func (t *tracker) advisorNote(seq uint64, ev event.Event) {
	e, _ := ev.(event.AdvisorNote)
	args := newOut()
	args.open()
	args.str("advisor", e.Advisor)
	args.str("severity", e.Severity)
	args.str("delivery", e.Delivery)
	args.close()
	raw, err := args.bytes()
	if err != nil {
		t.log.Warn("rpc: encode advisor note", "thread", t.threadID, "error", err)
	}
	t.startComplete(t.currentTurn(), &Item{
		Type: kindDynamic, ID: fmt.Sprintf("advisor-%d", seq), Tool: "advisor", Arguments: raw, Status: statusCompleted, Output: e.Note,
	})
	t.sink.notify("dalgona/advisor/note", func() writer {
		return AdvisorNoteNotification{ThreadID: t.threadID, Advisor: e.Advisor, Severity: e.Severity, Note: e.Note, Delivery: e.Delivery}
	})
}

func (t *tracker) threadNamed(_ uint64, ev event.Event) {
	e, _ := ev.(event.ThreadNamed)
	t.sink.notify("thread/name/updated", func() writer {
		return ThreadNameUpdatedNotification{ThreadID: t.threadID, ThreadName: e.Name}
	})
}

func (t *tracker) threadArchived(_ uint64, ev event.Event) {
	e, _ := ev.(event.ThreadArchived)
	method := "thread/unarchived"
	if e.Archived {
		method = "thread/archived"
	}
	t.sink.notify(method, func() writer { return ThreadRevertedNotification{ThreadID: t.threadID} })
}

func (t *tracker) branch(uint64, event.Event) {
	t.sink.notify("thread/reverted", func() writer { return ThreadRevertedNotification{ThreadID: t.threadID} })
}
