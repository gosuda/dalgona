package rpcserver

import (
	"cmp"
	"encoding/json/jsontext"
	"fmt"
	"log/slog"
	"math"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/textdiff"
)

const (
	reasoningSuffix     = "-reasoning"
	methodTurnStarted   = "turn/started"
	methodTurnCompleted = "turn/completed"
	methodItemStarted   = "item/started"
	methodItemCompleted = "item/completed"
	fieldText           = "text"
	fieldContent        = "content"
	fieldArguments      = "arguments"
)

type fieldBlobs map[string]*event.BlobRef

type sink interface {
	turn(method string, turn rpc.Turn)
	item(method, turnID string, it *rpc.Item)
	notify(method string, build func() rpc.Encoder)
}

type tracker struct {
	threadID   string
	created    event.SessionCreated
	load       func(*event.BlobRef) ([]byte, error)
	childCwd   func(id string) string
	keywords   func(threadID, text string, kind agent.InputKind) []agent.KeywordMatch
	log        *slog.Logger
	sink       sink
	turn       string
	turnEffort *string
	lastTurn   string
	turnAt     time.Time
	items      map[string]*rpc.Item
	calls      map[string]event.ToolCall
	classes    map[string]string
	approvals  map[string]string
	declined   map[string]bool
	callAt     map[string]time.Time
	usage      rpc.ThreadTokenUsage
	price      func(modelID string) (model.Price, bool)
	modelID    string
	cost       costTally
	seq        uint64
	statusSeq  uint64
	deferred   bool
	blobs      map[string]fieldBlobs
}

type costTally struct {
	micros   int64
	priced   bool
	unpriced bool
}

func newTracker(threadID string, load func(*event.BlobRef) ([]byte, error), childCwd func(string) string, keywords func(threadID, text string, kind agent.InputKind) []agent.KeywordMatch, log *slog.Logger) *tracker {
	return &tracker{
		threadID: threadID, load: load, childCwd: childCwd, keywords: keywords, log: log,
		items: map[string]*rpc.Item{}, calls: map[string]event.ToolCall{}, classes: map[string]string{},
		approvals: map[string]string{}, declined: map[string]bool{},
		callAt: map[string]time.Time{}, blobs: map[string]fieldBlobs{},
	}
}

type eventHandler func(t *tracker, seq uint64, ev event.Event)

var eventHandlers = map[string]eventHandler{
	event.TypeSessionCreated:    func(t *tracker, _ uint64, ev event.Event) { t.created, _ = ev.(event.SessionCreated) },
	event.TypeTurnStarted:       (*tracker).turnStarted,
	event.TypeTurnEffort:        (*tracker).resolvedEffort,
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

type sourcedCollector struct {
	*collector
	tracker *tracker
	kept    map[string]fieldBlobs
}

func (c sourcedCollector) item(method, turnID string, it *rpc.Item) {
	if refs := c.tracker.blobs[it.ID]; refs != nil {
		c.kept[it.ID] = refs
	}
	c.collector.item(method, turnID, it)
}

func (t *tracker) collectSourced(events []event.Seqd) ([]rpc.Turn, map[string]fieldBlobs) {
	c := sourcedCollector{collector: &collector{}, tracker: t, kept: map[string]fieldBlobs{}}
	t.sink = c
	for _, e := range events {
		t.apply(e.Seq, e.Event)
	}
	return c.result(), c.kept
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

func (t *tracker) itemText(itemID, field, text string, blob *event.BlobRef) string {
	if blob == nil || !t.deferred {
		return t.text(text, blob)
	}
	t.keepBlob(itemID, field, blob)
	return text
}

func (t *tracker) itemArgs(call event.ToolCall) jsontext.Value {
	if call.ArgsBlob == nil || !t.deferred {
		return t.args(call.Args, call.ArgsBlob)
	}
	t.keepBlob(call.ID, fieldArguments, call.ArgsBlob)
	return call.Args
}

func (t *tracker) keepBlob(itemID, field string, blob *event.BlobRef) {
	refs := t.blobs[itemID]
	if refs == nil {
		refs = fieldBlobs{}
		t.blobs[itemID] = refs
	}
	refs[field] = blob
}

func (t *tracker) setStatus(kind string, flags ...string) {
	seq := t.seq
	if seq <= t.statusSeq {
		seq = t.statusSeq + 1
	}
	t.statusSeq = seq
	status := rpc.ThreadStatus{Type: kind, ActiveFlags: flags}
	t.sink.notify("thread/status/changed", func() rpc.Encoder {
		return rpc.ThreadStatusChangedNotification{ThreadID: t.threadID, Status: status, Seq: seq}
	})
}

func (t *tracker) startComplete(turnID string, it *rpc.Item) {
	t.sink.item(methodItemStarted, turnID, it)
	t.complete(turnID, it)
}

func (t *tracker) complete(turnID string, it *rpc.Item) {
	t.sink.item(methodItemCompleted, turnID, it)
	delete(t.blobs, it.ID)
}

func (t *tracker) turnStarted(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnStarted)
	t.beginTurn(e)
	t.sink.turn(methodTurnStarted, rpc.Turn{ID: e.TurnID, Status: rpc.StatusInProgress, StartedAt: unixSeconds(e.StartedAt)})
	t.setStatus("active")
}

func (t *tracker) turnCompleted(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnCompleted)
	turn := rpc.Turn{ID: e.TurnID, Status: e.Status, StartedAt: unixSeconds(t.turnAt), CompletedAt: unixSeconds(e.CompletedAt), DurationMs: durationMs(t.turnAt, e.CompletedAt)}
	if e.Error != "" {
		turn.Error = &rpc.TurnError{Message: e.Error}
		t.sink.notify("error", func() rpc.Encoder {
			return rpc.ErrorNotification{ThreadID: t.threadID, TurnID: e.TurnID, Error: rpc.TurnError{Message: e.Error}}
		})
	}
	t.endTurn()
	t.sink.turn(methodTurnCompleted, turn)
	t.setStatus("idle")
}

func (t *tracker) beginTurn(e event.TurnStarted) {
	t.turn, t.lastTurn, t.turnAt = e.TurnID, e.TurnID, e.StartedAt
	t.turnEffort = nil
}

func (t *tracker) resolvedEffort(_ uint64, ev event.Event) {
	e, _ := ev.(event.TurnEffort)
	if t.turn == "" || e.TurnID != t.turn || t.turnEffort != nil && *t.turnEffort == e.Effort {
		return
	}
	t.turnEffort = &e.Effort
	t.sink.notify("dalgona/turn/effort", func() rpc.Encoder {
		return rpc.TurnEffortNotification{ThreadID: t.threadID, TurnID: e.TurnID, Effort: e.Effort}
	})
}

func (t *tracker) endTurn() {
	t.turn = ""
	t.turnEffort = nil
	t.turnAt = time.Time{}
	clear(t.items)
	clear(t.blobs)
}

func (t *tracker) userMessage(_ uint64, ev event.Event) {
	e, _ := ev.(event.UserMessage)
	t.modelID = cmp.Or(e.Model, t.modelID)
	text := t.itemText(e.ItemID, fieldText, e.Text, e.TextBlob)
	item := &rpc.Item{Type: rpc.KindUser, ID: e.ItemID, Text: text, Keywords: t.noticeMatches(e, text), Images: itemImages(e.Images)}
	t.startComplete(e.TurnID, item)
}

func (t *tracker) noticeMatches(e event.UserMessage, text string) []rpc.KeywordMatch {
	if len(e.Notices) == 0 {
		return nil
	}
	if e.TextBlob != nil && t.deferred {
		text = t.text(e.Text, e.TextBlob)
	}
	kind := agent.InputKind(cmp.Or(e.Kind, string(agent.InputPrompt)))
	journaled := make(map[string]bool, len(e.Notices))
	for _, notice := range e.Notices {
		journaled[notice.Word] = true
	}
	var out []rpc.KeywordMatch
	for _, match := range t.keywords(t.threadID, text, kind) {
		if journaled[match.Word] {
			out = append(out, keywordMatchOf(match))
		}
	}
	return out
}

func (t *tracker) assistantMessage(_ uint64, ev event.Event) {
	e, _ := ev.(event.AssistantMessage)
	t.finishStream(e.TurnID, rpc.KindReasoning, e.ItemID+reasoningSuffix, e.Reasoning, e.ReasoningBlob)
	t.finishStream(e.TurnID, rpc.KindAgent, e.ItemID, e.Text, e.TextBlob)
	for _, call := range e.ToolCalls {
		t.calls[call.ID] = call
	}
	if !t.addUsage(e) {
		return
	}
	t.notifyUsage(e.TurnID)
}

func (t *tracker) notifyUsage(turnID string) {
	usage := t.usage
	t.sink.notify("thread/tokenUsage/updated", func() rpc.Encoder {
		return rpc.ThreadTokenUsageUpdatedNotification{ThreadID: t.threadID, TurnID: turnID, TokenUsage: usage}
	})
}

func (t *tracker) applyUsage(ev event.Event) {
	switch e := ev.(type) {
	case event.UserMessage:
		t.modelID = cmp.Or(e.Model, t.modelID)
	case event.AssistantMessage:
		t.addUsage(e)
	case event.Compaction:
		t.usage.EstimatedContextTokens = &e.TokensAfter
	}
}

func (t *tracker) addUsage(e event.AssistantMessage) bool {
	if e.ContextWindow > 0 {
		t.usage.ModelContextWindow = e.ContextWindow
	}
	if e.Usage == (event.Usage{}) {
		return false
	}
	t.usage.EstimatedContextTokens = nil
	last := rpc.TokenUsageBreakdown{
		InputTokens: e.Usage.Input, CachedInputTokens: e.Usage.CacheRead, CacheWriteInputTokens: e.Usage.CacheWrite,
		OutputTokens: e.Usage.Output, TotalTokens: e.Usage.Input + e.Usage.Output,
	}
	t.usage.Last = last
	t.usage.Total = t.usage.Total.Add(last)
	t.priceMessage(e.Usage)
	t.stampCost()
	return true
}

func (t *tracker) priceMessage(usage event.Usage) {
	var price model.Price
	var ok bool
	if t.price != nil {
		price, ok = t.price(cmp.Or(t.modelID, t.created.Model))
	}
	if !ok {
		t.cost.unpriced = true
		return
	}
	counts := model.TokenCounts{Input: usage.Input, Output: usage.Output, CacheRead: usage.CacheRead, CacheWrite: usage.CacheWrite}
	t.cost.micros += min(price.CostMicros(counts), math.MaxInt64-t.cost.micros)
	t.cost.priced = true
}

func (t *tracker) stampCost() {
	if !t.cost.priced {
		return
	}
	micros := t.cost.micros
	t.usage.CostUsdMicros = &micros
	t.usage.CostPartial = t.cost.unpriced
}

func (t *tracker) usageSnapshot() *rpc.ThreadTokenUsage {
	if t.usage.Last == (rpc.TokenUsageBreakdown{}) && t.usage.ModelContextWindow == 0 && t.usage.EstimatedContextTokens == nil {
		return nil
	}
	usage := t.usage
	return &usage
}

func (t *tracker) finishStream(turnID, kind, id, text string, blob *event.BlobRef) {
	text = t.itemText(id, activeField(kind), text, blob)
	it, open := t.items[id]
	if !open {
		if text == "" && t.blobs[id] == nil {
			return
		}
		it = &rpc.Item{Type: kind, ID: id}
		t.sink.item(methodItemStarted, turnID, it)
	}
	delete(t.items, id)
	it.Text = text
	t.complete(turnID, it)
}

func (t *tracker) stream(kind, id string) {
	if _, ok := t.items[id]; ok {
		return
	}
	it := &rpc.Item{Type: kind, ID: id}
	t.items[id] = it
	t.sink.item(methodItemStarted, t.currentTurn(), it)
}

func (t *tracker) agentDelta(itemID, delta string) {
	t.stream(rpc.KindAgent, itemID)
	turnID := t.currentTurn()
	t.sink.notify("item/agentMessage/delta", func() rpc.Encoder {
		return rpc.AgentMessageDeltaNotification{ThreadID: t.threadID, TurnID: turnID, ItemID: itemID, Delta: delta}
	})
}

func (t *tracker) reasoningDelta(itemID, delta string) {
	id := itemID + reasoningSuffix
	t.stream(rpc.KindReasoning, id)
	turnID := t.currentTurn()
	t.sink.notify("item/reasoning/textDelta", func() rpc.Encoder {
		return rpc.ReasoningTextDeltaNotification{ThreadID: t.threadID, TurnID: turnID, ItemID: id, Delta: delta}
	})
}

func (t *tracker) execOutput(callID, delta string) {
	it, open := t.items[callID]
	if !open || it.Type != rpc.KindCommand {
		return
	}
	turnID := t.currentTurn()
	t.sink.notify("item/commandExecution/outputDelta", func() rpc.Encoder {
		return rpc.CommandExecutionOutputDeltaNotification{ThreadID: t.threadID, TurnID: turnID, ItemID: callID, Delta: delta}
	})
}

func (t *tracker) retryWait(turnID string, wait time.Duration) {
	t.sink.notify("dalgona/retry/wait", func() rpc.Encoder {
		return rpc.RetryWaitNotification{ThreadID: t.threadID, TurnID: turnID, RemainingMS: wait.Milliseconds()}
	})
}

func (t *tracker) sandboxChosen(_ uint64, ev event.Event) {
	e, _ := ev.(event.SandboxChosen)
	choice := "accept"
	if e.Choice == event.SandboxChoiceDangerous {
		choice = "dangerouslyAccept"
	}
	t.sink.notify("dalgona/sandbox/chosen", func() rpc.Encoder {
		return rpc.SandboxChosenNotification{ThreadID: t.threadID, Cause: e.Cause, Choice: choice}
	})
}

func (t *tracker) toolItem(call event.ToolCall, class string, files []event.FileChange) *rpc.Item {
	it := &rpc.Item{Type: toolKind(call.Name, class), ID: call.ID, Tool: call.Name, Status: rpc.StatusInProgress}
	switch it.Type {
	case rpc.KindCommand:
		it.Arguments = t.args(call.Args, call.ArgsBlob)
		it.Command = argString(it.Arguments, "command")
		it.Cwd = cmp.Or(argString(it.Arguments, "cwd"), t.created.Cwd)
		return it
	case rpc.KindFileChange:
		it.Changes = t.fileChanges(files)
	case rpc.KindMCP:
		it.Server, it.Tool, _ = strings.Cut(call.Name, "__")
	}
	it.Arguments = t.itemArgs(call)
	return it
}

func (t *tracker) fileChanges(files []event.FileChange) []rpc.FileChange {
	changes := make([]rpc.FileChange, 0, len(files))
	for _, file := range files {
		kind := rpc.KindUpdate
		switch {
		case file.PreSHA == "":
			kind = rpc.KindAdd
		case file.PostSHA == "":
			kind = rpc.KindDelete
		}
		changes = append(changes, t.fileChange(file, kind))
	}
	return changes
}

func (t *tracker) fileChange(file event.FileChange, kind string) rpc.FileChange {
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

func (t *tracker) openCall(callID string) *rpc.Item {
	if it, ok := t.items[callID]; ok {
		return it
	}
	call := t.calls[callID]
	call.ID = callID
	it := t.toolItem(call, t.classes[callID], nil)
	if call.Name == "" {
		return it
	}
	t.items[callID] = it
	t.sink.item(methodItemStarted, t.currentTurn(), it)
	return it
}

func (t *tracker) toolStarted(_ uint64, ev event.Event) {
	e, _ := ev.(event.ToolStarted)
	call := event.ToolCall{ID: e.CallID, Name: e.Tool, Args: e.Args, ArgsBlob: e.ArgsBlob}
	fresh := t.toolItem(call, e.Class, e.Files)
	if !e.StartedAt.IsZero() {
		t.callAt[e.CallID] = e.StartedAt
	}
	if it, ok := t.items[e.CallID]; ok {
		*it = *fresh
		return
	}
	t.calls[e.CallID] = call
	t.items[e.CallID] = fresh
	t.sink.item(methodItemStarted, e.TurnID, fresh)
}

func (t *tracker) toolCompleted(_ uint64, ev event.Event) {
	e, _ := ev.(event.ToolCompleted)
	it := t.openCall(e.CallID)
	switch {
	case e.OK:
		it.Status = rpc.StatusCompleted
	case t.declined[e.CallID] && (it.Type == rpc.KindCommand || it.Type == rpc.KindFileChange):
		it.Status = rpc.StatusDeclined
	default:
		it.Status = rpc.StatusFailed
	}
	it.Output, it.ExitCode = e.Output, e.ExitCode
	it.Spill = outputSpill(e.Spill)
	t.finishDuration(it, e.CallID, e.CompletedAt)
	turnID := t.currentTurn()
	argsBlob := t.blobs[e.CallID][fieldArguments]
	t.complete(turnID, it)
	if it.Tool == "todo" && e.OK {
		t.plan(turnID, t.args(it.Arguments, argsBlob))
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

func (t *tracker) finishDuration(it *rpc.Item, callID string, completed time.Time) {
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
	t.sink.notify("turn/plan/updated", func() rpc.Encoder {
		return rpc.TurnPlanUpdatedNotification{ThreadID: t.threadID, TurnID: turnID, Plan: steps}
	})
}

func todoSteps(args jsontext.Value) []rpc.TurnPlanStep {
	steps := []rpc.TurnPlanStep{}
	_ = decodeParams(args, func(name string, d *jsontext.Decoder) error {
		if name != "items" {
			return d.SkipValue()
		}
		return rpc.ReadArray(d, func() error {
			var step rpc.TurnPlanStep
			err := rpc.ReadMembers(d, func(name string, d *jsontext.Decoder) error {
				switch name {
				case "text":
					v, err := rpc.ReadString(d)
					step.Step = v
					return err
				case "status":
					v, err := rpc.ReadString(d)
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
		return rpc.StatusInProgress
	case "done":
		return rpc.StatusCompleted
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
	t.sink.notify("serverRequest/resolved", func() rpc.Encoder {
		return rpc.ServerRequestResolvedNotification{ThreadID: t.threadID, RequestID: e.RequestID, Resolution: agent.ApprovalResolution(e.Decision, e.Reason)}
	})
	if t.turn != "" {
		t.setStatus("active")
	}
}

func (t *tracker) compaction(seq uint64, ev event.Event) {
	e, _ := ev.(event.Compaction)
	t.applyUsage(e)
	turnID := t.currentTurn()
	t.startComplete(turnID, &rpc.Item{Type: rpc.KindCompaction, ID: fmt.Sprintf("compaction-%d", seq)})
	t.notifyUsage(turnID)
}

func (t *tracker) modelRetry(_ uint64, ev event.Event) {
	e, _ := ev.(event.ModelRetry)
	message := fmt.Sprintf("model request failed (%s, status %d); retry %d in %d ms", e.Verdict, e.Status, e.Attempt, e.DelayMS)
	t.sink.notify("error", func() rpc.Encoder {
		return rpc.ErrorNotification{ThreadID: t.threadID, TurnID: e.TurnID, WillRetry: true, Error: rpc.TurnError{Message: message}}
	})
}

func (t *tracker) childSpawned(_ uint64, ev event.Event) {
	e, _ := ev.(event.ChildSpawned)
	t.sink.notify("dalgona/thread/childSpawned", func() rpc.Encoder {
		return rpc.ChildSpawnedNotification{ThreadID: t.threadID, ChildID: e.ChildID, Cwd: cmp.Or(t.childCwd(e.ChildID), t.created.Cwd)}
	})
}

func (t *tracker) advisorNote(seq uint64, ev event.Event) {
	e, _ := ev.(event.AdvisorNote)
	args := rpc.NewWriter()
	args.Open()
	args.Str("advisor", e.Advisor)
	args.Str("severity", e.Severity)
	args.Str("delivery", e.Delivery)
	args.Close()
	raw, err := args.Bytes()
	if err != nil {
		t.log.Warn("rpc: encode advisor note", "thread", t.threadID, "error", err)
	}
	t.startComplete(t.currentTurn(), &rpc.Item{
		Type: rpc.KindDynamic, ID: fmt.Sprintf("advisor-%d", seq), Tool: "advisor", Arguments: raw, Status: rpc.StatusCompleted, Output: e.Note,
	})
	t.sink.notify("dalgona/advisor/note", func() rpc.Encoder {
		return rpc.AdvisorNoteNotification{ThreadID: t.threadID, Advisor: e.Advisor, Severity: e.Severity, Note: e.Note, Delivery: e.Delivery}
	})
}

func (t *tracker) threadNamed(_ uint64, ev event.Event) {
	e, _ := ev.(event.ThreadNamed)
	t.sink.notify("thread/name/updated", func() rpc.Encoder {
		return rpc.ThreadNameUpdatedNotification{ThreadID: t.threadID, ThreadName: e.Name}
	})
}

func (t *tracker) threadArchived(_ uint64, ev event.Event) {
	e, _ := ev.(event.ThreadArchived)
	method := "thread/unarchived"
	if e.Archived {
		method = "thread/archived"
	}
	t.sink.notify(method, func() rpc.Encoder { return rpc.ThreadRevertedNotification{ThreadID: t.threadID} })
}

func (t *tracker) branch(uint64, event.Event) {
	t.sink.notify("thread/reverted", func() rpc.Encoder { return rpc.ThreadRevertedNotification{ThreadID: t.threadID} })
}
