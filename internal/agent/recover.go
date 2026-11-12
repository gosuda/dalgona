package agent

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"path/filepath"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const (
	execInterrupted  = "interrupted: the host restarted while this command ran; it may have partly run"
	remoteInterrupt  = "interrupted: the tool may have partly run"
	editReplayed     = "the edit was applied before the host restarted"
	recoveredReplay  = "replayed"
	recoveredStopped = "interrupted"
)

type approvalRecord struct {
	requestID string
	resolved  bool
	decision  string
	reason    string
}

func (w *worker) recoverTurn(ctx context.Context) {
	if w.recoverScriptTurn(ctx) {
		return
	}
	if err := w.prime(ctx); err != nil {
		w.end(ctx, outcome{status: statusFailed, failure: err.Error()})
		return
	}
	if err := w.recoverCalls(ctx); err != nil {
		w.end(ctx, failure(ctx, err))
		return
	}
	w.drive(ctx)
}

func (w *worker) recoverCalls(ctx context.Context) error {
	v, err := w.t.view()
	if err != nil {
		return err
	}
	if err = w.use(ctx, v); err != nil {
		return err
	}
	for _, call := range lastCalls(v) {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		if err = w.recoverCall(ctx, v, call); err != nil {
			return err
		}
		if v, err = w.t.view(); err != nil {
			return err
		}
	}
	return nil
}

func lastCalls(v view) []event.ToolCall {
	for i := len(v.records) - 1; i >= 0; i-- {
		switch value := v.records[i].Event.(type) {
		case event.AssistantMessage:
			return value.ToolCalls
		case event.TurnStarted:
			return nil
		}
	}
	return nil
}

func (w *worker) recoverCall(ctx context.Context, v view, call event.ToolCall) error {
	if started, ok := v.state.InFlight[call.ID]; ok {
		return w.recoverStarted(ctx, v, started)
	}
	if _, pending := v.state.PendingCalls[call.ID]; !pending {
		return nil
	}
	return w.recoverPending(ctx, call, approvalFor(v, call.ID))
}

func (w *worker) recoverPending(ctx context.Context, call event.ToolCall, record approvalRecord) error {
	switch {
	case record.requestID != "" && !record.resolved:
		return w.runCall(ctx, call, record.requestID)
	case record.resolved && record.decision != "accept" && record.decision != "accept_session":
		_, err := w.t.appendEvents(ctx, event.ToolCompleted{CallID: call.ID, Output: record.reason, Recovered: "none"})
		return err
	default:
		return w.skip(ctx, []event.ToolCall{call}, skipRestarted)
	}
}

func approvalFor(v view, callID string) approvalRecord {
	var record approvalRecord
	for _, item := range v.records {
		switch value := item.Event.(type) {
		case event.ApprovalRequested:
			if value.CallID == callID {
				record = approvalRecord{requestID: value.RequestID}
			}
		case event.ApprovalResolved:
			if record.requestID != "" && value.RequestID == record.requestID {
				record.resolved, record.decision, record.reason = true, value.Decision, value.Reason
			}
		}
	}
	return record
}

func (w *worker) finishRecovered(ctx context.Context, p *prepared, recovered string) error {
	completed, err := w.execute(ctx, p)
	if err != nil {
		return err
	}
	completed.Recovered = recovered
	_, err = w.t.appendEvents(ctx, completed)
	return err
}

func (w *worker) recoverStarted(ctx context.Context, v view, state event.ToolState) error {
	if state.Tool == asktool.Name {
		return w.completeRecovered(ctx, state.CallID, asktool.TextRestarted, recoveredStopped)
	}
	item, ok := w.tools.byName[state.Tool]
	if !ok {
		return w.completeRecovered(ctx, state.CallID, remoteInterrupt, recoveredStopped)
	}
	args, err := w.t.host.loadRaw(state.Args, state.ArgsBlob)
	if err != nil {
		return err
	}
	p := &prepared{call: event.ToolCall{ID: state.CallID, Name: state.Tool}, tool: item, class: item.Class(), args: args}
	p.verdict = verdict{allow: true, unsandboxed: w.settings.Approval == config.ApprovalYolo}
	switch item.Class() {
	case tool.ClassExec:
		return w.recoverExec(ctx, p)
	case tool.ClassEdit:
		return w.recoverEdit(ctx, v, p)
	case tool.ClassRemote:
		return w.completeRecovered(ctx, state.CallID, remoteInterrupt, recoveredStopped)
	case tool.ClassScript:
		return w.recoverScript(ctx, v, p)
	case tool.ClassRead, tool.ClassAgent:
		return w.finishRecovered(ctx, p, recoveredReplay)
	}
	return w.completeRecovered(ctx, state.CallID, remoteInterrupt, recoveredStopped)
}

func (w *worker) recoverScript(ctx context.Context, v view, p *prepared) error {
	if err := w.recoverNested(ctx, v, p.call.ID+"/"); err != nil {
		return err
	}
	return w.finishRecovered(ctx, p, recoveredReplay)
}

func (w *worker) recoverNested(ctx context.Context, v view, prefix string) error {
	for id := range v.state.InFlight {
		if !strings.HasPrefix(id, prefix) {
			continue
		}
		current, err := w.t.view()
		if err != nil {
			return err
		}
		if nested, open := current.state.InFlight[id]; open {
			if err = w.recoverStarted(ctx, current, nested); err != nil {
				return err
			}
		}
	}
	return nil
}

func (w *worker) completeRecovered(ctx context.Context, callID, output, recovered string) error {
	_, err := w.t.appendEvents(ctx, event.ToolCompleted{CallID: callID, Output: output, Recovered: recovered})
	return err
}

func (w *worker) recoverExec(ctx context.Context, p *prepared) error {
	out, data, err := w.readProcess(ctx, p.processID())
	if errors.Is(err, runner.ErrProcessNotFound) {
		return w.completeRecovered(ctx, p.call.ID, execInterrupted, recoveredStopped)
	}
	if err != nil {
		return w.completeRecovered(ctx, p.call.ID, execInterrupted+"\n"+err.Error(), recoveredStopped)
	}
	if out.Alive && out.Exit == nil {
		return w.finishRecovered(ctx, p, recoveredReplay)
	}
	projected, _ := tool.Project(data, tool.KeepTail, out.Path)
	result := tool.Result{Output: []byte(projected), Path: out.Path}
	recovered := recoveredReplay
	if out.Exit != nil {
		result.OK, result.ExitCode = out.Exit.Code == 0, &out.Exit.Code
	} else {
		result.Output, recovered = []byte(joinError(projected, execInterrupted)), recoveredStopped
	}
	completed, err := w.completion(ctx, p, result, nil)
	if err != nil {
		return err
	}
	completed.Recovered = recovered
	_, err = w.t.appendEvents(ctx, completed)
	return err
}

func (w *worker) readProcess(ctx context.Context, id string) (runner.Output, []byte, error) {
	var data []byte
	var offset int64
	for {
		out, err := w.t.runner.Read(ctx, id, offset)
		if err != nil {
			return out, data, err
		}
		data = append(data, out.Data...)
		if len(out.Data) == 0 || out.Next <= offset {
			return out, data, nil
		}
		offset = out.Next
	}
}

func (w *worker) recoverEdit(ctx context.Context, v view, p *prepared) error {
	files := startedFiles(v, p.call.ID)
	if len(files) == 0 {
		return w.replanEdit(ctx, p)
	}
	current := make([]string, len(files))
	allPre, allPost := true, true
	for i, file := range files {
		sha, err := w.fileSHA(ctx, file.Path)
		if err != nil {
			return err
		}
		current[i] = sha
		allPre = allPre && sha == file.PreSHA
		allPost = allPost && sha == file.PostSHA
	}
	switch {
	case allPost:
		_, err := w.t.appendEvents(ctx, event.ToolCompleted{CallID: p.call.ID, OK: true, Output: editReplayed, Recovered: recoveredReplay})
		return err
	case allPre:
		return w.replanEdit(ctx, p)
	default:
		return w.restoreEdit(ctx, p.call.ID, files, current)
	}
}

func startedFiles(v view, callID string) []event.FileChange {
	for _, item := range v.records {
		if started, ok := item.Event.(event.ToolStarted); ok && started.CallID == callID {
			return started.Files
		}
	}
	return nil
}

func (w *worker) replanEdit(ctx context.Context, p *prepared) error {
	editor, ok := p.tool.(tool.Editor)
	if !ok {
		return w.finishRecovered(ctx, p, recoveredReplay)
	}
	plan, err := editor.Plan(ctx, p.toolCall(), w.env(p.verdict))
	if err != nil {
		return w.completeRecovered(ctx, p.call.ID, err.Error(), recoveredReplay)
	}
	p.plan = &plan
	return w.finishRecovered(ctx, p, recoveredReplay)
}

func (w *worker) fileSHA(ctx context.Context, path string) (string, error) {
	data, err := w.t.runner.ReadFile(ctx, absPath(w.t.created.Cwd, path))
	if errors.Is(err, fs.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", fmt.Errorf("agent: read %s for recovery: %w", path, err)
	}
	return snapshot.Digest(data), nil
}

func (w *worker) restoreEdit(ctx context.Context, callID string, files []event.FileChange, current []string) error {
	var changed []string
	var targets []event.FileChange
	for i, file := range files {
		if current[i] == file.PreSHA {
			continue
		}
		changed = append(changed, file.Path)
		if current[i] != file.PostSHA {
			continue
		}
		targets = append(targets, file)
	}
	restored := make([][]byte, len(targets))
	for i, file := range targets {
		data, err := w.preImage(file)
		if err != nil {
			return err
		}
		restored[i] = data
	}
	for i, file := range targets {
		if err := w.writeRestored(ctx, file, restored[i]); err != nil {
			return err
		}
	}
	output := "interrupted: files changed during recovery: " + strings.Join(changed, ", ")
	return w.completeRecovered(ctx, callID, output, recoveredStopped)
}

func (w *worker) preImage(file event.FileChange) ([]byte, error) {
	if file.PreBlob == nil {
		return nil, nil
	}
	data, err := w.t.host.journal.Blob(file.PreBlob.Blob)
	if err != nil {
		return nil, fmt.Errorf("agent: load pre-image of %s: %w", file.Path, err)
	}
	raw, err := w.t.host.unmaskStrict(string(data))
	if err != nil {
		return nil, fmt.Errorf("agent: restore %s: %w", file.Path, err)
	}
	return []byte(raw), nil
}

func (w *worker) writeRestored(ctx context.Context, file event.FileChange, data []byte) error {
	path := absPath(w.t.created.Cwd, file.Path)
	if file.PreBlob == nil {
		if err := w.t.runner.Remove(ctx, path); err != nil && !errors.Is(err, fs.ErrNotExist) {
			return fmt.Errorf("agent: restore %s: %w", file.Path, err)
		}
		return nil
	}
	mode := fs.FileMode(0o644)
	if info, statErr := w.t.runner.Stat(ctx, path); statErr == nil {
		mode = info.Mode.Perm()
	} else if !errors.Is(statErr, fs.ErrNotExist) {
		return fmt.Errorf("agent: stat %s for recovery: %w", file.Path, statErr)
	}
	if err := w.t.runner.WriteFile(ctx, path, data, mode); err != nil {
		return fmt.Errorf("agent: restore %s: %w", file.Path, err)
	}
	return nil
}

func (h *Host) unmaskStrict(text string) (string, error) {
	if h.cfg.Auth == nil {
		return text, nil
	}
	out, err := h.cfg.Auth.UnmaskStrict(text)
	if err != nil {
		return "", fmt.Errorf("agent: restore secret placeholder: %w", err)
	}
	return out, nil
}

func absPath(cwd, path string) string {
	if filepath.IsAbs(path) {
		return path
	}
	return filepath.Join(cwd, path)
}
