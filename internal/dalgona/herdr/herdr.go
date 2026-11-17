package herdr

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os/exec"
	"strconv"
	"sync"
)

type State string

const (
	StateWorking State = "working"
	StateIdle    State = "idle"
	StateBlocked State = "blocked"
)

type Reporter struct {
	binary string
	paneID string
	failed sync.Once
}

func FromEnv(env func(string) string) *Reporter {
	if env == nil || env("HERDR_ENV") != "1" {
		return nil
	}
	paneID := env("HERDR_PANE_ID")
	if paneID == "" {
		return nil
	}
	binary := env("HERDR_BIN_PATH")
	if binary == "" {
		binary = "herdr"
	}
	return &Reporter{binary: binary, paneID: paneID}
}

func (r *Reporter) Report(ctx context.Context, state State, seq uint64, threadID string) {
	if r == nil {
		return
	}
	if !validState(state) {
		r.recordFailure(fmt.Errorf("invalid state %q", state))
		return
	}
	args := []string{
		"pane", "report-agent", r.paneID,
		"--source", "dalgona",
		"--agent", "dalgona",
		"--state", string(state),
		"--seq", strconv.FormatUint(seq, 10),
		"--agent-session-id", threadID,
		"--", "dalgona", "--resume", threadID,
	}
	if err := r.run(ctx, args); err != nil {
		r.recordFailure(err)
	}
}

func (r *Reporter) Release(ctx context.Context) {
	if r == nil {
		return
	}
	args := []string{
		"pane", "release-agent", r.paneID,
		"--source", "dalgona",
		"--agent", "dalgona",
	}
	if err := r.run(ctx, args); err != nil {
		r.recordFailure(err)
	}
}

func (r *Reporter) SubagentPane(ctx context.Context, childID, cwd string) {
	if r == nil {
		return
	}
	args := []string{"pane", "split", "--current", "--direction", "right", "--cwd", cwd, "--no-focus"}
	output, err := r.output(ctx, args)
	if err != nil {
		r.recordFailure(err)
		return
	}
	paneID, err := splitPaneID(output)
	if err != nil {
		r.recordFailure(err)
		return
	}
	runArgs := []string{"pane", "run", paneID, "dalgona --resume " + childID}
	if err := r.run(ctx, runArgs); err != nil {
		r.recordFailure(err)
	}
}

func (r *Reporter) run(ctx context.Context, args []string) error {
	cmd := exec.CommandContext(ctx, r.binary, args...)
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("herdr %s: %w", args[1], err)
	}
	return nil
}

func (r *Reporter) output(ctx context.Context, args []string) ([]byte, error) {
	cmd := exec.CommandContext(ctx, r.binary, args...)
	output, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("herdr %s: %w", args[1], err)
	}
	return output, nil
}

func (r *Reporter) recordFailure(err error) {
	r.failed.Do(func() {
		slog.Error("herdr: command failed", "error", err)
	})
}

func validState(state State) bool {
	switch state {
	case StateWorking, StateIdle, StateBlocked:
		return true
	default:
		return false
	}
}

func splitPaneID(data []byte) (string, error) {
	result, err := objectField(data, "result")
	if err != nil {
		return "", fmt.Errorf("herdr split response: %w", err)
	}
	pane, err := objectField(result, "pane")
	if err != nil {
		return "", fmt.Errorf("herdr split response: %w", err)
	}
	id, err := stringField(pane, "pane_id")
	if err != nil {
		return "", fmt.Errorf("herdr split response: %w", err)
	}
	if id == "" {
		return "", errors.New("herdr split response: pane_id is empty")
	}
	return id, nil
}

func objectField(data []byte, want string) (jsontext.Value, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(data))
	if decoder.PeekKind() != jsontext.KindBeginObject {
		return nil, errors.New("expected an object")
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, err
	}
	for {
		token, err := decoder.ReadToken()
		if err != nil {
			return nil, err
		}
		if token.Kind() == jsontext.KindEndObject {
			return nil, fmt.Errorf("missing field %q", want)
		}
		key := token.String()
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, err
		}
		if key == want {
			return value, nil
		}
	}
}

func stringField(data []byte, want string) (string, error) {
	value, err := objectField(data, want)
	if err != nil {
		return "", err
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	if decoder.PeekKind() != jsontext.KindString {
		return "", fmt.Errorf("field %q is not a string", want)
	}
	token, err := decoder.ReadToken()
	if err != nil {
		return "", err
	}
	text := token.String()
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		if err == nil {
			return "", fmt.Errorf("field %q has trailing data", want)
		}
		return "", err
	}
	return text, nil
}
