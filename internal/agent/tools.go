package agent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"html"
	"io/fs"
	"regexp"
	"sync"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	loopGuardLimit   = 4
	loopGuardMessage = "repeated call: this exact call already ran 4 times in a row; change approach"
	skipInterrupted  = "the turn was interrupted"
	skipRestarted    = "the host restarted before this call ran"
)

var instructionsPath = regexp.MustCompile(`<instructions path="([^"]*)">`)

type prepared struct {
	call    event.ToolCall
	tool    tool.Tool
	class   tool.Class
	args    jsontext.Value
	repairs int
	plan    *tool.Plan
	verdict verdict
}

func classString(class tool.Class) string {
	switch class {
	case tool.ClassRead:
		return "read"
	case tool.ClassEdit:
		return "edit"
	case tool.ClassExec:
		return "exec"
	case tool.ClassAgent:
		return "agent"
	case tool.ClassScript:
		return "script"
	case tool.ClassRemote:
		return "remote"
	}
	return "read"
}

func (w *worker) runCalls(ctx context.Context, calls []event.ToolCall) error {
	for i := 0; i < len(calls); {
		if ctx.Err() != nil {
			return w.skip(ctx, calls[i:], skipInterrupted)
		}
		batch := w.agentRun(calls[i:])
		if batch > 1 {
			if err := w.runAgentBatch(ctx, calls[i:i+batch]); err != nil {
				return err
			}
			i += batch
			continue
		}
		if err := w.runCall(ctx, calls[i], ""); err != nil {
			return err
		}
		i++
	}
	return nil
}

func (w *worker) agentRun(calls []event.ToolCall) int {
	count := 0
	for _, call := range calls {
		item, ok := w.t.tools[call.Name]
		if !ok || item.Class() != tool.ClassAgent {
			break
		}
		count++
	}
	return count
}

func (w *worker) skip(ctx context.Context, calls []event.ToolCall, reason string) error {
	events := make([]event.Event, 0, len(calls))
	for _, call := range calls {
		events = append(events, event.ToolCompleted{CallID: call.ID, Output: "skipped: " + reason, Recovered: "skipped"})
	}
	_, err := w.t.appendEvents(ctx, events...)
	return err
}

func (w *worker) runCall(ctx context.Context, call event.ToolCall, requestID string) error {
	p, err := w.prepare(ctx, call, requestID)
	if err != nil || p == nil {
		return err
	}
	completed, err := w.execute(ctx, p)
	if err != nil {
		return err
	}
	_, err = w.t.appendEvents(ctx, completed)
	return err
}

func (w *worker) runAgentBatch(ctx context.Context, calls []event.ToolCall) error {
	ready := make([]*prepared, 0, len(calls))
	for i, call := range calls {
		if ctx.Err() != nil {
			if err := w.skip(ctx, calls[i:], skipInterrupted); err != nil {
				return err
			}
			break
		}
		p, err := w.prepare(ctx, call, "")
		if err != nil {
			return err
		}
		if p != nil {
			ready = append(ready, p)
		}
	}
	results := make([]event.ToolCompleted, len(ready))
	errs := make([]error, len(ready))
	var group sync.WaitGroup
	for i, p := range ready {
		group.Go(func() { results[i], errs[i] = w.execute(ctx, p) })
	}
	group.Wait()
	if err := errors.Join(errs...); err != nil {
		return err
	}
	events := make([]event.Event, len(results))
	for i, completed := range results {
		events[i] = completed
	}
	_, err := w.t.appendEvents(ctx, events...)
	return err
}

func (w *worker) prepare(ctx context.Context, call event.ToolCall, requestID string) (*prepared, error) {
	by := "mode:" + w.t.created.Approval
	item, ok := w.t.tools[call.Name]
	if !ok {
		return nil, w.declineCall(ctx, call, requestID, decline(by, "Tool "+call.Name+" not found"))
	}
	raw, err := w.t.host.loadRaw(call.Args, call.ArgsBlob)
	if err != nil {
		return nil, err
	}
	args, repairs, err := repairArgs(item.Spec(), raw)
	if err != nil {
		return nil, w.declineCall(ctx, call, requestID, decline(by, "invalid arguments for "+call.Name+": "+err.Error()))
	}
	repeated, err := w.repeated(call.Name, args)
	if err != nil {
		return nil, err
	}
	if repeated {
		return nil, w.declineCall(ctx, call, requestID, decline("rule:loop-guard", loopGuardMessage))
	}
	p := &prepared{call: call, tool: item, class: item.Class(), args: args, repairs: repairs}
	return w.admit(ctx, p, requestID)
}

func (w *worker) admit(ctx context.Context, p *prepared, requestID string) (*prepared, error) {
	gc, err := w.gateCall(ctx, p, requestID)
	if err != nil {
		return nil, w.failBeforeRun(ctx, p, requestID, err)
	}
	v, err := w.gate(ctx, gc)
	if err != nil {
		return nil, err
	}
	if v.cancel {
		w.t.Interrupt()
	}
	if !v.allow {
		_, err := w.t.appendEvents(ctx, event.ToolCompleted{CallID: p.call.ID, Output: v.reason, Recovered: "none"})
		return nil, err
	}
	p.verdict = v
	return p, w.start(ctx, p)
}

func (w *worker) gateCall(ctx context.Context, p *prepared, requestID string) (gateCall, error) {
	gc := gateCall{call: p.call, args: p.args, class: p.class, requestID: requestID}
	if p.class == tool.ClassExec {
		fields, err := readArgFields(p.args)
		if err != nil {
			return gc, fmt.Errorf("invalid arguments for %s: %w", p.call.Name, err)
		}
		gc.command, gc.escalate, gc.justification = fields.command, fields.escalate, fields.justification
	}
	editor, ok := p.tool.(tool.Editor)
	if !ok {
		return gc, nil
	}
	plan, err := editor.Plan(ctx, tool.Call{ID: p.call.ID, Name: p.call.Name, Args: p.args}, w.env(verdict{}))
	if err != nil {
		return gc, err
	}
	p.plan = &plan
	for _, file := range plan.Files {
		gc.paths = append(gc.paths, file.Path)
	}
	gc.outside = w.t.host.outsideRoots(w.t.created.Cwd, gc.paths)
	return gc, nil
}

func (w *worker) declineCall(ctx context.Context, call event.ToolCall, requestID string, v verdict) error {
	events := make([]event.Event, 0, 3)
	if requestID == "" {
		requestID = newID("req-")
		events = append(events, event.ApprovalRequested{RequestID: requestID, CallID: call.ID, Summary: truncate(call.Name, summaryLimit)})
	}
	events = append(events,
		event.ApprovalResolved{RequestID: requestID, Decision: v.decision, By: v.by, Reason: v.reason},
		event.ToolCompleted{CallID: call.ID, Output: v.reason, Recovered: "none"},
	)
	_, err := w.t.appendEvents(ctx, events...)
	return err
}

func (w *worker) failBeforeRun(ctx context.Context, p *prepared, requestID string, cause error) error {
	if requestID != "" {
		return w.declineCall(ctx, p.call, requestID, decline("mode:"+w.t.created.Approval, cause.Error()))
	}
	started, err := w.started(p, nil)
	if err != nil {
		return err
	}
	_, err = w.t.appendEvents(ctx, started, event.ToolCompleted{CallID: p.call.ID, Output: cause.Error(), Recovered: "none"})
	return err
}

func (w *worker) start(ctx context.Context, p *prepared) error {
	var files []tool.FileChange
	if p.plan != nil {
		files = p.plan.Files
	}
	started, err := w.started(p, files)
	if err != nil {
		return err
	}
	_, err = w.t.appendEvents(ctx, started)
	return err
}

func (w *worker) started(p *prepared, files []tool.FileChange) (event.ToolStarted, error) {
	args, blob, err := w.t.host.rawPayload(p.args)
	if err != nil {
		return event.ToolStarted{}, err
	}
	started := event.ToolStarted{
		CallID: p.call.ID, TurnID: w.turnID, Tool: p.call.Name, Args: args, ArgsBlob: blob,
		Class: classString(p.class), Repairs: p.repairs,
	}
	for _, file := range files {
		change := event.FileChange{Path: file.Path, PreSHA: file.PreSHA, PostSHA: file.PostSHA}
		if file.PreSHA != "" {
			sha, err := w.t.host.journal.PutBlob(file.Pre)
			if err != nil {
				return event.ToolStarted{}, fmt.Errorf("agent: store pre-image of %s: %w", file.Path, err)
			}
			change.PreBlob = &event.BlobRef{Blob: sha, Bytes: int64(len(file.Pre))}
		}
		started.Files = append(started.Files, change)
	}
	return started, nil
}

func repairArgs(spec tool.Spec, raw jsontext.Value) (jsontext.Value, int, error) {
	if spec.Freeform || len(spec.Schema) == 0 {
		return raw, 0, nil
	}
	return tool.Repair(spec.Schema, raw)
}

func (w *worker) execute(ctx context.Context, p *prepared) (event.ToolCompleted, error) {
	var result tool.Result
	var err error
	if p.plan != nil {
		result, err = p.plan.Apply(ctx)
	} else {
		result, err = p.tool.Run(ctx, tool.Call{ID: p.call.ID, Name: p.call.Name, Args: p.args}, w.env(p.verdict))
	}
	return w.completion(ctx, p, result, err)
}

func (w *worker) env(v verdict) tool.Env {
	mode := runner.SandboxWorkspaceWrite
	if v.unsandboxed || w.t.created.Sandbox == string(config.SandboxOff) {
		mode = runner.SandboxOff
	}
	roots := append([]string{w.t.created.Cwd}, w.t.host.roots...)
	return tool.Env{
		Runner: w.t.runner, Cwd: w.t.created.Cwd, Home: w.t.host.cfg.Config.Home,
		Sandbox: runner.Sandbox{Mode: mode, WritableRoots: roots}, Masker: w.t.host.masker(), Snapshots: w.t.snapshots,
	}
}

func (w *worker) completion(ctx context.Context, p *prepared, result tool.Result, runErr error) (event.ToolCompleted, error) {
	ok := result.OK && runErr == nil
	output := string(result.Output)
	if ok && p.call.Name == "read" {
		output += w.nestedInstructions(p.args)
	}
	if runErr != nil {
		output = joinError(output, runErr.Error())
	}
	runPath := ""
	if p.class == tool.ClassExec {
		runPath = result.Path
	}
	text, spill, err := w.project(ctx, p.tool.Spec().Keep, w.t.host.mask(output), runPath)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	if err := w.storeSnapshots(result.Meta); err != nil {
		return event.ToolCompleted{}, err
	}
	return event.ToolCompleted{
		CallID: p.call.ID, OK: ok, Output: text, Spill: spill, ExitCode: result.ExitCode, Recovered: "none", Meta: result.Meta,
	}, nil
}

func joinError(output, message string) string {
	if output == "" {
		return message
	}
	return output + "\n" + message
}

func (w *worker) project(ctx context.Context, keep tool.Keep, masked, runPath string) (string, *event.Spill, error) {
	if runPath != "" {
		if spill, err := w.runSpill(ctx, runPath); err != nil || spill != nil {
			return fitTail(masked, event.MaxProjectionBytes), spill, err
		}
	}
	if len(masked) <= event.MaxInlineBytes {
		return masked, nil, nil
	}
	sha, err := w.t.host.journal.PutBlob([]byte(masked))
	if err != nil {
		return "", nil, fmt.Errorf("agent: spill tool output: %w", err)
	}
	path := "blobs/" + sha
	text, _ := tool.Project([]byte(masked), keep, path)
	return fitTail(text, event.MaxProjectionBytes), &event.Spill{Path: path, Bytes: int64(len(masked)), SHA256: sha}, nil
}

func (w *worker) runSpill(ctx context.Context, path string) (*event.Spill, error) {
	info, err := w.t.runner.Stat(ctx, path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("agent: stat exec output %s: %w", path, err)
	}
	if info.Size <= event.MaxInlineBytes {
		return nil, nil
	}
	data, err := w.t.runner.ReadFile(ctx, path)
	if err != nil {
		return nil, fmt.Errorf("agent: read exec spill %s: %w", path, err)
	}
	sum := sha256.Sum256(data)
	return &event.Spill{Path: path, Bytes: int64(len(data)), SHA256: hex.EncodeToString(sum[:])}, nil
}

func fitTail(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	tail := text[len(text)-limit:]
	for len(tail) > 0 && !utf8.RuneStart(tail[0]) {
		tail = tail[1:]
	}
	return tail
}

func (w *worker) storeSnapshots(meta jsontext.Value) error {
	refs, err := metaRefs(meta)
	if err != nil {
		return fmt.Errorf("agent: tool metadata: %w", err)
	}
	for _, ref := range refs {
		entry, err := w.t.snapshots.Get(ref)
		if err != nil {
			w.t.host.log.Warn("agent: tool metadata names an unknown snapshot", "thread", w.t.id, "ref", ref, "error", err)
			continue
		}
		if _, err := w.t.host.journal.PutBlob(entry.Data); err != nil {
			return fmt.Errorf("agent: store read snapshot %s: %w", ref, err)
		}
	}
	return nil
}

func metaRefs(meta jsontext.Value) ([]string, error) {
	trimmed := bytes.TrimSpace(meta)
	if len(trimmed) == 0 || trimmed[0] != '{' {
		return nil, nil
	}
	var refs []string
	decoder := jsontext.NewDecoder(bytes.NewReader(trimmed))
	err := model.ReadObjectFields(decoder, func(name string) error {
		switch name {
		case "ref":
			ref, err := model.ReadString(decoder)
			refs = append(refs, ref)
			return err
		case "captures":
			return model.ReadArrayItems(decoder, func(int) error {
				return readCaptureRef(decoder, &refs)
			})
		default:
			return decoder.SkipValue()
		}
	})
	return refs, err
}

func readCaptureRef(decoder *jsontext.Decoder, refs *[]string) error {
	return model.ReadObjectFields(decoder, func(name string) error {
		if name != "ref" {
			return decoder.SkipValue()
		}
		ref, err := model.ReadString(decoder)
		*refs = append(*refs, ref)
		return err
	})
}

func (w *worker) nestedInstructions(args jsontext.Value) string {
	fields, err := readArgFields(args)
	if err != nil || fields.path == "" {
		return ""
	}
	seen, err := w.seenInstructions()
	if err != nil {
		w.t.host.log.Warn("agent: nested instructions", "thread", w.t.id, "error", err)
		return ""
	}
	text, _ := prompt.Nested(w.t.created.Cwd, fields.path, seen)
	if text == "" {
		return ""
	}
	return "\n\n" + text
}

func (w *worker) seenInstructions() (map[string]bool, error) {
	v, err := w.t.view()
	if err != nil {
		return nil, err
	}
	kept, _ := afterCompaction(event.ActivePath(v.records))
	seen := make(map[string]bool)
	for _, item := range kept {
		completed, ok := item.Event.(event.ToolCompleted)
		if !ok {
			continue
		}
		for _, match := range instructionsPath.FindAllStringSubmatch(completed.Output, -1) {
			seen[html.UnescapeString(match[1])] = true
		}
	}
	return seen, nil
}

func (w *worker) repeated(name string, args jsontext.Value) (bool, error) {
	v, err := w.t.view()
	if err != nil {
		return false, err
	}
	key := callKey(name, args)
	path := event.ActivePath(v.records)
	count := 0
	for i := len(path) - 1; i >= 0 && count < loopGuardLimit; i-- {
		switch value := path[i].Event.(type) {
		case event.UserMessage:
			return false, nil
		case event.ToolStarted:
			startedArgs, err := w.t.host.loadRaw(value.Args, value.ArgsBlob)
			if err != nil {
				return false, err
			}
			if callKey(value.Tool, startedArgs) != key {
				return false, nil
			}
			count++
		}
	}
	return count >= loopGuardLimit, nil
}

func callKey(name string, args jsontext.Value) string {
	canonical := jsontext.Value(bytes.Clone(args))
	if err := canonical.Canonicalize(); err != nil {
		return name + "\x00" + string(args)
	}
	return name + "\x00" + string(canonical)
}

type argFields struct {
	command       string
	justification string
	path          string
	escalate      bool
}

func readArgFields(args jsontext.Value) (argFields, error) {
	var out argFields
	decoder := jsontext.NewDecoder(bytes.NewReader(args))
	err := model.ReadObjectFields(decoder, func(name string) error {
		var err error
		switch name {
		case "command":
			out.command, err = model.ReadString(decoder)
		case "justification":
			out.justification, err = model.ReadString(decoder)
		case "path":
			out.path, err = model.ReadString(decoder)
		case "escalate":
			out.escalate, err = model.ReadBool(decoder)
		default:
			err = decoder.SkipValue()
		}
		return err
	})
	return out, err
}

func (h *Host) mask(text string) string {
	if h.cfg.Auth == nil {
		return text
	}
	return h.cfg.Auth.Mask(text)
}

type identityMasker struct{}

func (identityMasker) Mask(text string) string   { return text }
func (identityMasker) Unmask(text string) string { return text }

func (h *Host) masker() tool.Masker {
	if h.cfg.Auth == nil {
		return identityMasker{}
	}
	return h.cfg.Auth.Masker()
}
