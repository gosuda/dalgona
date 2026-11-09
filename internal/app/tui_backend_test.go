package app

import (
	"bytes"
	"context"
	"errors"
	"io"
	"strings"
	"testing"
)

type stubConn struct {
	*bytes.Buffer
	closed int
}

func (s *stubConn) Close() error {
	s.closed++
	return nil
}

func TestInteractiveRequiresWiredTerminal(t *testing.T) {
	conn := &stubConn{Buffer: &bytes.Buffer{}}
	env := CLIEnv{
		Dial: func(context.Context) (io.ReadWriteCloser, error) { return conn, nil },
	}
	err := env.interactive(t.Context(), cliFlags{})
	if err == nil || !strings.Contains(err.Error(), "not wired") {
		t.Fatalf("interactive without a backend = %v, want a not-wired error", err)
	}
	if conn.closed != 1 {
		t.Fatalf("dialed connection closed %d times, want 1", conn.closed)
	}
}

func TestInteractivePassesFlagsAndStdioToBackend(t *testing.T) {
	backendErr := errors.New("backend stopped")
	conn := &stubConn{Buffer: &bytes.Buffer{}}
	var stdin, stdout bytes.Buffer
	var gotConn io.ReadWriteCloser
	var gotOpts TUIOptions
	env := CLIEnv{
		Dial:   func(context.Context) (io.ReadWriteCloser, error) { return conn, nil },
		Stdin:  &stdin,
		Stdout: &stdout,
		TUI: func(_ context.Context, c io.ReadWriteCloser, opts TUIOptions) error {
			gotConn = c
			gotOpts = opts
			return backendErr
		},
	}
	flags := cliFlags{resume: "thread-x", model: "model-a", approval: "ask", sandbox: "workspace-write"}
	if err := env.interactive(t.Context(), flags); !errors.Is(err, backendErr) {
		t.Fatalf("interactive error = %v, want %v", err, backendErr)
	}
	if gotConn != conn {
		t.Fatal("backend did not receive the dialed connection")
	}
	if gotOpts.ThreadID != "thread-x" {
		t.Fatalf("backend thread = %q, want thread-x", gotOpts.ThreadID)
	}
	if gotOpts.Model != "model-a" {
		t.Fatalf("backend model = %q, want model-a", gotOpts.Model)
	}
	if gotOpts.Approval != "ask" {
		t.Fatalf("backend approval = %q, want ask", gotOpts.Approval)
	}
	if gotOpts.Sandbox != "workspace-write" {
		t.Fatalf("backend sandbox = %q, want workspace-write", gotOpts.Sandbox)
	}
	if gotOpts.Stdin != io.Reader(&stdin) {
		t.Fatal("backend did not receive the configured stdin")
	}
	if gotOpts.Stdout != io.Writer(&stdout) {
		t.Fatal("backend did not receive the configured stdout")
	}
}

func TestInteractiveForwardsStatusAndClosesConn(t *testing.T) {
	conn := &stubConn{Buffer: &bytes.Buffer{}}
	var got Status
	env := CLIEnv{
		Dial: func(context.Context) (io.ReadWriteCloser, error) { return conn, nil },
		OnStatus: func(s Status) {
			got = s
		},
		TUI: func(_ context.Context, _ io.ReadWriteCloser, opts TUIOptions) error {
			opts.OnStatus(Status{State: "running", Seq: 7, ThreadID: "thread-x"})
			return nil
		},
	}
	if err := env.interactive(t.Context(), cliFlags{}); err != nil {
		t.Fatalf("interactive error = %v", err)
	}
	if want := (Status{State: "running", Seq: 7, ThreadID: "thread-x"}); got != want {
		t.Fatalf("forwarded status = %+v, want %+v", got, want)
	}
	if conn.closed != 1 {
		t.Fatalf("dialed connection closed %d times, want 1", conn.closed)
	}
}
