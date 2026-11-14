package tui

import (
	"context"
	"encoding/json/v2"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

type shellCaller struct {
	fakeCaller
	result rpc.CommandExecResponse
}

func (f *shellCaller) Call(ctx context.Context, method string, params, result any) error {
	if method != "command/exec" {
		return f.fakeCaller.Call(ctx, method, params, result)
	}
	f.calls = append(f.calls, callRecord{method: method, params: params})
	out, ok := result.(*rpc.CommandExecResponse)
	if !ok {
		return errors.New("unexpected command/exec result")
	}
	*out = f.result
	return nil
}

func shellType(t *testing.T, m model, text string) model {
	t.Helper()
	for _, r := range text {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	return m
}

func shellSubmit(t *testing.T, m model) model {
	t.Helper()
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command != nil {
		m, _ = drive(t, m, command())
	}
	return m
}

func shellCalls(caller *shellCaller) []string {
	var calls []string
	for _, call := range caller.calls {
		if call.method != "command/exec" {
			continue
		}
		raw, _ := json.Marshal(call.params)
		calls = append(calls, string(raw))
	}
	return calls
}

func TestBangRunsCommandExecAndAddsChip(t *testing.T) {
	t.Parallel()
	caller := &shellCaller{result: rpc.CommandExecResponse{Stdout: "hi\n"}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m = shellSubmit(t, shellType(t, m, "!echo hi"))
	calls := shellCalls(caller)
	if len(calls) != 1 {
		t.Fatalf("command/exec calls = %d, want 1: %v", len(calls), calls)
	}
	if !strings.Contains(calls[0], `"command":["sh","-c","echo hi"]`) {
		t.Fatalf("command/exec params = %s, want argv sh -c echo hi", calls[0])
	}
	if got := m.input.composer.firstLine(); got != "[Shell #1 exit 0, 1 line]" {
		t.Fatalf("prompt = %q, want the shell chip", got)
	}
	if m.input.mode != inputShell {
		t.Fatalf("mode after Enter = %v, want shell: only esc leaves shell mode", m.input.mode)
	}
}

type deadlineCaller struct {
	shellCaller
	deadline time.Time
}

func (f *deadlineCaller) Call(ctx context.Context, method string, params, result any) error {
	f.deadline, _ = ctx.Deadline()
	return f.shellCaller.Call(ctx, method, params, result)
}

func TestShellCommandDeadlineOutlastsItsTimeout(t *testing.T) {
	t.Parallel()
	caller := &deadlineCaller{}
	m := newTestModel(caller, Options{Cwd: "/work"})
	shellSubmit(t, shellType(t, m, "!sleep 100"))
	if remaining := time.Until(caller.deadline); remaining <= shellTimeoutMS*time.Millisecond {
		t.Fatalf("command/exec deadline is %v from now, want more than the %d ms command timeout", remaining, shellTimeoutMS)
	}
}

func TestShellExpansionFencesAndSpans(t *testing.T) {
	t.Parallel()
	cases := []struct{ command, output, want string }{
		{"cat notes", "a\n```\nb", "Output of `cat notes` (exit 0):\n````\na\n```\nb\n````"},
		{"echo `date`", "x", "Output of `` echo `date` `` (exit 0):\n```\nx\n```"},
	}
	for _, c := range cases {
		if got := shellExpansion(c.command, 0, c.output); got != c.want {
			t.Fatalf("shellExpansion(%q, %q) = %q, want %q", c.command, c.output, got, c.want)
		}
	}
}

func TestShellEscOnEmptyLineLeavesMode(t *testing.T) {
	t.Parallel()
	caller := &shellCaller{}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m, _ = drive(t, m, press('!', 0, "!"))
	if m.input.mode != inputShell {
		t.Fatalf("mode after ! = %v, want shell", m.input.mode)
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.input.mode != inputPrompt {
		t.Fatalf("mode after esc = %v, want prompt", m.input.mode)
	}
	if calls := shellCalls(caller); len(calls) != 0 {
		t.Fatalf("esc ran a command: %v", calls)
	}
}

func TestShellFailedCommandShowsFailedRowAndChipOutput(t *testing.T) {
	t.Parallel()
	caller := &shellCaller{result: rpc.CommandExecResponse{ExitCode: 3, Stdout: "a\n", Stderr: "boom\n"}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m = shellSubmit(t, shellType(t, m, "!make x"))
	row := lastEntry(t, m)
	for _, want := range []string{"Failed", "exit 3", "make x"} {
		if !strings.Contains(row, want) {
			t.Fatalf("row = %q, want it to contain %q", row, want)
		}
	}
	if got := m.input.composer.firstLine(); got != "[Shell #1 exit 3, 2 lines]" {
		t.Fatalf("prompt = %q, want the failed chip", got)
	}
	want := "Output of `make x` (exit 3):\n```\na\nboom\n```"
	if got := m.input.composer.Expanded(); got != want {
		t.Fatalf("expanded chip = %q, want %q", got, want)
	}
}
