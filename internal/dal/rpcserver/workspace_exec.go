package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"runtime"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const (
	methodWorkspaceExec   = "dalgona/workspace/exec"
	workspaceExecDeadline = 125 * time.Second
	workspaceExecTimeout  = 120 * time.Second
	workspaceExecOutput   = 64 << 10
	workspaceExecLimit    = 4
	workspaceExecText     = 64 << 10
)

var workspaceExecPolicy = textinput.Policy{Lines: textinput.MultiLine, Limit: textinput.Limit{Unit: textinput.Bytes, Max: workspaceExecText}}

type workspaceExecParams struct {
	WorkspaceID string `json:"workspaceId"`
	Text        string `json:"text"`
}

type workspaceExecResult struct {
	ExitCode        *int   `json:"exitCode"`
	DurationMs      int64  `json:"durationMs"`
	OutputBase64    []byte `json:"outputBase64"`
	OutputTruncated bool   `json:"outputTruncated"`
	Termination     string `json:"termination"`
}

func (s *server) workspaceExec(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var p workspaceExecParams
	if err := json.Unmarshal(params, &p); err != nil || p.WorkspaceID == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if p.Text == "" {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	use, err := s.workspaceUse(ctx, p.WorkspaceID, rightTerminal, true)
	if err != nil {
		return nil, err
	}
	defer use.release()
	if verifyErr := verifyPinned(ctx, use.handle); verifyErr != nil {
		return nil, verifyErr
	}
	if !s.workspace.startCommand() {
		return nil, rpc.NativeKind(rpc.ErrorLimitExceeded)
	}
	defer s.workspace.endCommand()
	thread, err := s.host.Resume(ctx, use.handle.threadID)
	if err != nil || thread.Runner() != use.handle.run {
		return nil, rpc.NativeKind(rpc.ErrorExpired)
	}
	return s.runCommand(ctx, thread, use.handle, p.Text)
}

func verifyPinned(ctx context.Context, h *workspaceHandle) error {
	pinned, ok := h.files.(runner.WorkspaceHandle)
	if !ok {
		return nil
	}
	if err := pinned.VerifyExec(ctx); err != nil {
		return rpc.NativeKind(rpc.ErrorExpired)
	}
	return nil
}

func (s *server) runCommand(ctx context.Context, thread *agent.Thread, h *workspaceHandle, text string) (jsontext.Value, error) {
	started := time.Now()
	spec := agent.ExecSpec{Argv: shellArgv(h.remote, text), Timeout: workspaceExecTimeout, OutputCap: workspaceExecOutput}
	ran, err := thread.Exec(ctx, "workspace-"+newHandle(), spec)
	outcome := workspaceExecResult{
		DurationMs: time.Since(started).Milliseconds(), OutputBase64: ran.Output, OutputTruncated: ran.CapReached, Termination: "exited",
	}
	switch {
	case errors.Is(err, agent.ErrExecNotStarted) && ctx.Err() != nil:
		return nil, rpc.NativeKind(rpc.ErrorCancelled)
	case errors.Is(err, agent.ErrExecNotStarted):
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	case err != nil:
		return nil, rpc.NativeKind(rpc.ErrorUnknownOutcome)
	case ran.TimedOut:
		outcome.Termination = "timedOut"
	case ctx.Err() != nil:
		outcome.Termination, outcome.ExitCode = "cancelled", &ran.ExitCode
	default:
		outcome.ExitCode = &ran.ExitCode
	}
	return json.Marshal(outcome)
}

func shellArgv(remote bool, text string) []string {
	if !remote && runtime.GOOS == "windows" {
		return runner.WindowsShellArgv(text)
	}
	return []string{"sh", "-c", text}
}

func (t *workspaceTable) startCommand() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.commands >= workspaceExecLimit {
		return false
	}
	t.commands++
	return true
}

func (t *workspaceTable) endCommand() {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.commands--
}
