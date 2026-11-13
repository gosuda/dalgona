package scripthost

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"log/slog"
	"math"
	"strconv"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/tool/task"
)

const recoveredInterrupted = "interrupted"

var errNoJudge = errors.New("scripthost: no judge is configured")

type effects struct {
	t     *agent.Thread
	judge JudgeFactory
	log   *slog.Logger
	idx   *effectCache
}

type effectKey struct {
	run string
	n   uint64
}

type toolTrace struct {
	started      event.ToolStarted
	completed    event.ToolCompleted
	hasStarted   bool
	hasCompleted bool
}

type effectCache struct {
	mu          sync.Mutex
	head        int
	turn        string
	starts      map[string]event.ScriptStarted
	completions map[string]event.ScriptCompleted
	effects     map[effectKey]event.ScriptEffect
	tools       map[string]toolTrace
}

func newEffectCache() *effectCache {
	return &effectCache{
		starts:      map[string]event.ScriptStarted{},
		completions: map[string]event.ScriptCompleted{},
		effects:     map[effectKey]event.ScriptEffect{},
		tools:       map[string]toolTrace{},
	}
}

func callID(run string, n int) string { return run + "/" + strconv.Itoa(n) }
func turnStart(records []event.Seqd) (string, int) {
	for i := len(records) - 1; i >= 0; i-- {
		if started, ok := records[i].Event.(event.TurnStarted); ok {
			return started.TurnID, i
		}
	}
	return "", 0
}

func scopedRecords(records []event.Seqd) []event.Seqd {
	_, pos := turnStart(records)
	if pos < 0 || pos > len(records) {
		return records
	}
	return records[pos:]
}

func (c *effectCache) ensureTurn(records []event.Seqd) {
	turn, pos := turnStart(records)
	if c.turn != turn {
		c.starts = map[string]event.ScriptStarted{}
		c.completions = map[string]event.ScriptCompleted{}
		c.effects = map[effectKey]event.ScriptEffect{}
		c.tools = map[string]toolTrace{}
		c.turn = turn
		c.head = pos
	}
	if c.head < 0 {
		c.head = 0
	}
	if c.head > len(records) {
		c.head = len(records)
	}
	if pos > c.head {
		c.head = pos
	}
}

func orNull(value jsontext.Value) jsontext.Value {
	if len(value) == 0 {
		return jsontext.Value("null")
	}
	return value
}

func quoteJSON(text string) jsontext.Value {
	out, err := jsontext.AppendQuote(nil, text)
	if err != nil {
		out, _ = jsontext.AppendQuote(nil, string(bytes.ToValidUTF8([]byte(text), []byte("\uFFFD"))))
	}
	return out
}

func effectIndex(n int) uint64 {
	if n < 0 {
		return 0
	}
	return uint64(n)
}

func (fx effects) Started(start script.RunStart) (script.RunStart, *script.Outcome, error) {
	records, state, err := fx.t.Records()
	if err != nil {
		return script.RunStart{}, nil, err
	}
	if started, ok := fx.idx.start(records, start.ID); ok {
		return fx.resumeStarted(records, state, started, start)
	}
	source, err := fx.t.PutBlob(start.Source)
	if err != nil {
		return script.RunStart{}, nil, err
	}
	args, argsBlob, err := fx.t.StoreRaw(orNull(start.Args))
	if err != nil {
		return script.RunStart{}, nil, err
	}
	err = fx.t.Append(context.Background(), event.ScriptStarted{
		RunID: start.ID, SourceBlob: source, Entry: start.Entry, Args: args, ArgsBlob: argsBlob, Seed: uint64(max(start.Seed, 0)),
	})
	return start, nil, err
}

func (fx effects) resumeStarted(records []event.Seqd, state event.State, started event.ScriptStarted, start script.RunStart) (script.RunStart, *script.Outcome, error) {
	if _, open := state.Scripts[start.ID]; open {
		pinned, err := fx.pinned(started)
		if err != nil {
			return script.RunStart{}, nil, err
		}
		return pinned, nil, nil
	}
	done, ok := fx.idx.completed(records, start.ID)
	if !ok {
		return script.RunStart{}, nil, fmt.Errorf("scripthost: duplicate run id %s", start.ID)
	}
	pinned, err := fx.pinned(started)
	if err != nil {
		return script.RunStart{}, nil, err
	}
	if !sameRequest(pinned, start) {
		return script.RunStart{}, nil, fmt.Errorf("scripthost: duplicate run id %s", start.ID)
	}
	outcome, err := fx.recordedOutcome(done)
	if err != nil {
		return script.RunStart{}, nil, fmt.Errorf("scripthost: outcome of %s: %w", start.ID, err)
	}
	return script.RunStart{}, &outcome, nil
}

func sameRequest(pinned, start script.RunStart) bool {
	return pinned.Entry == start.Entry && bytes.Equal(pinned.Source, start.Source) && bytes.Equal(pinned.Args, orNull(start.Args))
}

func (fx effects) recordedOutcome(done event.ScriptCompleted) (script.Outcome, error) {
	if done.OK {
		value, err := fx.t.LoadRaw(done.Value, done.ValueBlob)
		if err != nil {
			return script.Outcome{}, err
		}
		return script.Outcome{Status: script.StatusCompleted, Value: value}, nil
	}
	if done.Status == "" {
		return script.Outcome{Status: script.StatusFailed, Error: script.Failure{Message: done.Error}}, nil
	}
	return script.Outcome{Status: script.Status(done.Status), Error: script.Failure{Code: done.Code, Message: done.Error, Stage: done.Stage}}, nil
}

func (fx effects) pinned(started event.ScriptStarted) (script.RunStart, error) {
	source, err := fx.t.Blob(started.SourceBlob)
	if err != nil {
		return script.RunStart{}, fmt.Errorf("scripthost: source of %s: %w", started.RunID, err)
	}
	args, err := fx.t.LoadRaw(started.Args, started.ArgsBlob)
	if err != nil {
		return script.RunStart{}, err
	}
	if started.Seed > math.MaxInt64 {
		return script.RunStart{}, fmt.Errorf("scripthost: seed of %s is out of range", started.RunID)
	}
	seed := int64(started.Seed)
	return script.RunStart{ID: started.RunID, Entry: started.Entry, Source: source, Args: args, Seed: seed}, nil
}

func (fx effects) Lookup(run string, n int) (script.Recorded, bool) {
	records, _, err := fx.t.Records()
	if err != nil {
		fx.log.Warn("scripthost: lookup on an unreadable thread", "thread", fx.t.ID(), "run", run, "error", err)
		return script.Recorded{Interrupted: true}, true
	}
	return fx.recorded(fx.idx.trace(records, run, n))
}

type effectTrace struct {
	effect    *event.ScriptEffect
	started   *event.ToolStarted
	completed *event.ToolCompleted
}

func traceEffect(records []event.Seqd, run string, n int) effectTrace {
	id := callID(run, n)
	index := effectIndex(n)
	var trace effectTrace
	for _, item := range scopedRecords(records) {
		switch value := item.Event.(type) {
		case event.ScriptEffect:
			if value.RunID == run && value.N == index {
				trace.effect = &value
			}
		case event.ToolStarted:
			if value.CallID == id {
				trace.started = &value
			}
		case event.ToolCompleted:
			if value.CallID == id {
				trace.completed = &value
			}
		}
	}
	return trace
}

func (c *effectCache) observe(records []event.Seqd) {
	for _, item := range records[min(c.head, len(records)):] {
		switch value := item.Event.(type) {
		case event.ScriptStarted:
			if _, ok := c.starts[value.RunID]; !ok {
				c.starts[value.RunID] = value
			}
		case event.ScriptCompleted:
			c.completions[value.RunID] = value
		case event.ScriptEffect:
			c.effects[effectKey{run: value.RunID, n: value.N}] = value
		case event.ToolStarted:
			trace := c.tools[value.CallID]
			trace.started = value
			trace.hasStarted = true
			c.tools[value.CallID] = trace
		case event.ToolCompleted:
			trace := c.tools[value.CallID]
			trace.completed = value
			trace.hasCompleted = true
			c.tools[value.CallID] = trace
		}
	}
	c.head = len(records)
}

func (c *effectCache) start(records []event.Seqd, run string) (event.ScriptStarted, bool) {
	if c == nil {
		return scanStart(records, run)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.ensureTurn(records)
	c.observe(records)
	started, ok := c.starts[run]
	return started, ok
}

func (c *effectCache) completed(records []event.Seqd, run string) (event.ScriptCompleted, bool) {
	if c == nil {
		return scanCompleted(records, run)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.ensureTurn(records)
	c.observe(records)
	done, ok := c.completions[run]
	return done, ok
}

func scanCompleted(records []event.Seqd, run string) (event.ScriptCompleted, bool) {
	for _, item := range scopedRecords(records) {
		if done, ok := item.Event.(event.ScriptCompleted); ok && done.RunID == run {
			return done, true
		}
	}
	return event.ScriptCompleted{}, false
}

func scanStart(records []event.Seqd, run string) (event.ScriptStarted, bool) {
	for _, item := range scopedRecords(records) {
		if started, ok := item.Event.(event.ScriptStarted); ok && started.RunID == run {
			return started, true
		}
	}
	return event.ScriptStarted{}, false
}

func (c *effectCache) trace(records []event.Seqd, run string, n int) effectTrace {
	if c == nil {
		return traceEffect(records, run, n)
	}
	id := callID(run, n)
	index := effectIndex(n)
	c.mu.Lock()
	defer c.mu.Unlock()
	c.ensureTurn(records)
	c.observe(records)
	var trace effectTrace
	if effect, ok := c.effects[effectKey{run: run, n: index}]; ok {
		effect := effect
		trace.effect = &effect
	}
	if tool, ok := c.tools[id]; ok {
		if tool.hasStarted {
			started := tool.started
			trace.started = &started
		}
		if tool.hasCompleted {
			completed := tool.completed
			trace.completed = &completed
		}
	}
	return trace
}

func (fx effects) recorded(trace effectTrace) (script.Recorded, bool) {
	switch {
	case trace.effect != nil:
		result, err := fx.t.LoadRaw(trace.effect.Result, trace.effect.ResultBlob)
		return script.Recorded{Op: trace.effect.Op, ArgsSHA: trace.effect.ArgsSHA, Result: result, Error: trace.effect.Error, Denied: trace.effect.Denied, Interrupted: err != nil}, true
	case trace.started != nil:
		return fx.recordedTool(*trace.started, trace.completed), true
	default:
		return script.Recorded{}, false
	}
}

func (fx effects) recordedTool(started event.ToolStarted, completed *event.ToolCompleted) script.Recorded {
	args, err := fx.t.LoadRaw(started.Args, started.ArgsBlob)
	rec := script.Recorded{Op: script.ToolOp(started.Tool), ArgsSHA: script.ArgsSHA(args)}
	switch {
	case err != nil || completed == nil || completed.Recovered == recoveredInterrupted:
		rec.Interrupted = true
	case completed.OK:
		rec.Result = quoteJSON(completed.Output)
	default:
		rec.Error = cmp.Or(completed.Output, "the tool failed")
	}
	return rec
}

func (fx effects) Record(slot script.Slot, result jsontext.Value) error {
	return fx.recordResult(slot, result)
}

func (fx effects) recordResult(slot script.Slot, result jsontext.Value) error {
	value, blob, err := fx.t.StoreRaw(result)
	if err != nil {
		return err
	}
	return fx.t.Append(context.Background(), event.ScriptEffect{
		RunID: slot.Run, N: effectIndex(slot.N), Op: slot.Op, ArgsSHA: slot.ArgsSHA,
		Result: value, ResultBlob: blob,
	})
}

func (fx effects) recordDenial(slot script.Slot, reason string) error {
	return fx.t.Append(context.Background(), event.ScriptEffect{
		RunID: slot.Run, N: effectIndex(slot.N), Op: slot.Op, ArgsSHA: slot.ArgsSHA,
		Error: reason, Denied: true,
	})
}

func (fx effects) Completed(run string, outcome script.Outcome) error {
	_, state, err := fx.t.Records()
	if err != nil {
		return err
	}
	if _, open := state.Scripts[run]; !open {
		return nil
	}
	done := event.ScriptCompleted{RunID: run, OK: outcome.Status == script.StatusCompleted, Status: string(outcome.Status), Code: outcome.Error.Code, Stage: outcome.Error.Stage}
	if done.OK {
		done.Value, done.ValueBlob, err = fx.t.StoreRaw(orNull(outcome.Value))
		if err != nil {
			return err
		}
	} else {
		done.Error = cmp.Or(outcome.Error.Message, string(outcome.Status))
	}
	return fx.t.Append(context.Background(), done)
}

func (fx effects) CallTool(ctx context.Context, slot script.Slot, name string, args jsontext.Value) (jsontext.Value, error) {
	call := event.ToolCall{ID: callID(slot.Run, slot.N), Name: name, Args: args}
	if err := fx.t.RunNestedCall(ctx, call); err != nil {
		return nil, err
	}
	records, _, err := fx.t.Records()
	if err != nil {
		return nil, err
	}
	return fx.callResult(slot, fx.idx.trace(records, slot.Run, slot.N))
}

func (fx effects) callResult(slot script.Slot, trace effectTrace) (jsontext.Value, error) {
	if trace.started == nil {
		reason := "the call was declined"
		if trace.completed != nil {
			reason = cmp.Or(trace.completed.Output, reason)
		}
		if err := fx.recordDenial(slot, reason); err != nil {
			return nil, err
		}
		return nil, fmt.Errorf("%w: %s", script.ErrDenied, reason)
	}
	rec := fx.recordedTool(*trace.started, trace.completed)
	switch {
	case rec.Interrupted:
		return nil, fmt.Errorf("scripthost: tool call %s was interrupted", trace.started.CallID)
	case rec.Error != "":
		return nil, errors.New(rec.Error)
	default:
		return rec.Result, nil
	}
}

func (fx effects) Spawn(ctx context.Context, slot script.Slot, prompt string, opts script.SpawnOpts) (string, error) {
	id, err := fx.t.StartChild(ctx, callID(slot.Run, slot.N), task.Request{Prompt: prompt, Runner: opts.Runner, Model: opts.Model})
	if err != nil {
		return "", err
	}
	return id, fx.recordResult(slot, quoteJSON(id))
}

func (fx effects) Wait(ctx context.Context, ids []string) ([]string, error) {
	texts := make([]string, len(ids))
	for i, id := range ids {
		text, err := fx.t.ChildText(ctx, id)
		if err != nil {
			return nil, err
		}
		texts[i] = text
	}
	return texts, nil
}

func (fx effects) Log(run, msg string) {
	fx.log.Info("scripthost: script log", "thread", fx.t.ID(), "run", run, "message", msg)
}
