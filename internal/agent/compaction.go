package agent

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/compact"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompts"
	"github.com/gosuda/dalgona/internal/model"
)

const (
	compactAuto            = "auto"
	compactManual          = "manual"
	compactOverflow        = "overflow"
	defaultThreshold       = 0.8
	defaultKeepTokens      = 20000
	pruneProtectTokens     = 40000
	pruneMinBytes          = 2000
	pruneMaxCount          = 20
	pruneTargetTokens      = 20000
	fileListLimit          = 20
	renderedOutputLimit    = 2000
	nothingToSummarizeText = "compaction: nothing to summarize"
)

var ErrNothingToSummarize = errors.New(nothingToSummarizeText)

type compactionView struct {
	path       []event.Seqd
	entries    []compact.Entry
	events     []event.Event
	prevIdx    int
	summary    string
	lastMarker uint64
}

func (t *Thread) compactionView(v view) compactionView {
	path := event.ActivePath(v.records)
	out := compactionView{path: path}
	var firstKept uint64
	for _, item := range path {
		if value, ok := item.Event.(event.Compaction); ok {
			firstKept, out.summary, out.lastMarker = value.FirstKeptSeq, value.Summary, item.Seq
		}
	}
	pruned := prunedCalls(path)
	for _, item := range path {
		if _, ok := item.Event.(event.Compaction); ok {
			continue
		}
		summarized := firstKept != 0 && item.Seq < firstKept
		if summarized {
			out.prevIdx = len(out.entries) + 1
		}
		entry := entryFor(item, v.sizes[item.Seq-1])
		if summarized || pruned[entry.CallID] {
			entry.OutputBytes = 0
		}
		out.entries = append(out.entries, entry)
		out.events = append(out.events, item.Event)
	}
	return out
}

func entryFor(item event.Seqd, size int) compact.Entry {
	entry := compact.Entry{Seq: item.Seq, Kind: compact.Other, Bytes: size}
	switch value := item.Event.(type) {
	case event.UserMessage:
		entry.Kind = compact.UserMessage
	case event.AssistantMessage:
		entry.Kind = compact.AssistantMessage
	case event.ToolStarted:
		entry.Kind = compact.ToolStarted
	case event.ToolCompleted:
		entry.Kind, entry.CallID, entry.OutputBytes = compact.ToolCompleted, value.CallID, len(value.Output)
	}
	return entry
}

func usedTokens(cv compactionView) int {
	tail := 0
	for i := len(cv.entries) - 1; i >= cv.prevIdx; i-- {
		message, ok := cv.events[i].(event.AssistantMessage)
		prompt := message.Usage.Input + message.Usage.CacheRead + message.Usage.CacheWrite
		if ok && cv.entries[i].Seq > cv.lastMarker && prompt > 0 {
			return prompt + tail/4
		}
		tail += cv.entries[i].Bytes
	}
	return (len(cv.summary) + tail) / 4
}

func (w *worker) autoCompact(ctx context.Context, v view) (bool, error) {
	caps := w.t.host.capabilities(ctx, w.registry, w.settings.Model)
	cv := w.t.compactionView(v)
	threshold := cmp.Or(w.t.host.cfg.Config.CompactionThreshold, defaultThreshold)
	if !compact.ShouldCompact(caps.Context, usedTokens(cv), threshold) {
		return false, nil
	}
	err := w.compact(ctx, compactAuto)
	if errors.Is(err, ErrNothingToSummarize) {
		return false, nil
	}
	return err == nil, err
}

func (w *worker) recoverOverflow(ctx context.Context) error {
	v, err := w.t.view()
	if err != nil {
		return err
	}
	cv := w.t.compactionView(v)
	ids := compact.PruneSelect(cv.entries, pruneProtectTokens, pruneMinBytes, pruneMaxCount, pruneTargetTokens)
	if len(ids) > 0 {
		if _, err = w.t.appendEvents(ctx, event.ContextPruned{TurnID: w.turnID, CallIDs: ids}); err != nil {
			return err
		}
	}
	err = w.compact(ctx, compactOverflow)
	if errors.Is(err, ErrNothingToSummarize) {
		return nil
	}
	return err
}

func (w *worker) compact(ctx context.Context, method string) error {
	v, err := w.t.view()
	if err != nil {
		return err
	}
	if err = w.use(v); err != nil {
		return err
	}
	cv := w.t.compactionView(v)
	keep := cmp.Or(w.t.host.cfg.Config.CompactionKeepTokens, defaultKeepTokens)
	cut, ok := compact.Cut(cv.entries, keep, cv.prevIdx)
	if !ok {
		w.t.host.logOnce(fmt.Sprintf("%s\x00%s\x00%d", nothingToSummarizeText, w.t.id, cv.lastMarker), nothingToSummarizeText, "thread", w.t.id)
		return ErrNothingToSummarize
	}
	before := usedTokens(cv)
	summary, err := w.summarize(ctx, cv, cut, v.state)
	if err != nil {
		return fmt.Errorf("compaction failed: %w", err)
	}
	after := len(summary) / 4
	for _, entry := range cv.entries[cut:] {
		after += entry.Bytes / 4
	}
	marker := event.Compaction{FirstKeptSeq: cv.entries[cut].Seq, Summary: summary, Method: method, TokensBefore: before, TokensAfter: after}
	_, err = w.t.appendEvents(ctx, marker)
	return err
}

func (w *worker) summarize(ctx context.Context, cv compactionView, cut int, state event.State) (string, error) {
	resolved, err := w.t.host.ResolveRole(ctx, "smol", w.settings.Model)
	if err != nil {
		return "", err
	}
	provider, modelID, err := model.ParseModelID(resolved)
	if err != nil {
		return "", err
	}
	input := w.renderSpan(cv, cut)
	req := model.Request{Model: modelID, System: prompts.Compaction, Messages: []model.Message{{Role: "user", Text: input}}}
	stream, err := w.registry.Stream(ctx, provider, req)
	if err != nil {
		return "", err
	}
	text, err := collectText(ctx, stream)
	if closeErr := stream.Close(); closeErr != nil {
		w.t.host.log.Debug("agent: close summarizer stream", "thread", w.t.id, "error", closeErr)
	}
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(text) == "" {
		return "", errEmptyResponse
	}
	return strings.TrimSpace(text) + w.todoText(state), nil
}

func collectText(ctx context.Context, stream model.Stream) (string, error) {
	var text strings.Builder
	for {
		next, err := stream.Next(ctx)
		if errors.Is(err, io.EOF) {
			return text.String(), nil
		}
		if err != nil {
			return "", err
		}
		switch value := next.(type) {
		case model.TextDelta:
			text.WriteString(value.Text)
		case model.Error:
			return "", modelFailure{failure: value}
		}
	}
}

func (w *worker) renderSpan(cv compactionView, cut int) string {
	var out strings.Builder
	if cv.summary != "" {
		out.WriteString("<previous-summary>\n" + cv.summary + "\n</previous-summary>\n\n")
	}
	out.WriteString("<conversation>\n")
	var read, changed []string
	for _, item := range cv.events[cv.prevIdx:cut] {
		w.renderEvent(&out, item)
		read, changed = trackFiles(item, read, changed)
	}
	out.WriteString("</conversation>\n")
	writeFileList(&out, "read-files", read)
	writeFileList(&out, "modified-files", changed)
	return out.String()
}

func (w *worker) renderEvent(out *strings.Builder, item event.Event) {
	switch value := item.(type) {
	case event.UserMessage:
		text, err := w.t.host.loadText(value.Text, value.TextBlob)
		w.logLoad(err)
		out.WriteString("[user] " + text + "\n")
	case event.AssistantMessage:
		text, err := w.t.host.loadText(value.Text, value.TextBlob)
		w.logLoad(err)
		out.WriteString("[assistant] " + text + "\n")
		for _, call := range value.ToolCalls {
			out.WriteString("[tool call " + call.Name + "] " + truncate(string(call.Args), renderedOutputLimit) + "\n")
		}
	case event.ToolCompleted:
		out.WriteString("[tool result] " + truncate(value.Output, renderedOutputLimit) + "\n")
	case event.AdvisorNote:
		out.WriteString("[advisor " + value.Severity + "] " + value.Note + "\n")
	}
}

func (w *worker) logLoad(err error) {
	if err != nil {
		w.t.host.log.Warn("agent: load text for compaction", "thread", w.t.id, "error", err)
	}
}

func trackFiles(item event.Event, read, changed []string) ([]string, []string) {
	started, ok := item.(event.ToolStarted)
	if !ok {
		return read, changed
	}
	for _, file := range started.Files {
		changed = appendUnique(changed, file.Path)
	}
	if started.Class == "read" {
		if fields, err := readArgFields(started.Args); err == nil && fields.path != "" {
			read = appendUnique(read, fields.path)
		}
	}
	return read, changed
}

func appendUnique(list []string, value string) []string {
	for _, existing := range list {
		if existing == value {
			return list
		}
	}
	if len(list) >= fileListLimit {
		return list
	}
	return append(list, value)
}

func writeFileList(out *strings.Builder, tag string, files []string) {
	if len(files) == 0 {
		return
	}
	out.WriteString("<" + tag + ">\n" + strings.Join(files, "\n") + "\n</" + tag + ">\n")
}

func (w *worker) todoText(state event.State) string {
	if len(state.Todos) == 0 {
		return ""
	}
	var out strings.Builder
	out.WriteString("\n\n<todo>\n")
	for _, item := range state.Todos {
		out.WriteString("- [" + item.Status + "] " + item.Text + "\n")
	}
	out.WriteString("</todo>")
	return out.String()
}
