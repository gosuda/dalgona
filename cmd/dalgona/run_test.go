package main

import (
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/daemon"
	"github.com/gosuda/dalgona/internal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}

type lockedBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

func startDaemon(t *testing.T, home string) {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	logs := &lockedBuffer{}
	done := make(chan int, 1)
	go func() {
		done <- run(ctx, []string{"daemon"}, stdio{in: strings.NewReader(""), out: logs, errOut: logs})
	}()
	t.Cleanup(func() {
		cancel()
		if code := <-done; code != 0 {
			t.Errorf("daemon exit = %d, logs:\n%s", code, logs.String())
		}
	})
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(20 * time.Millisecond) {
		if _, err := os.Stat(daemon.SocketPath(home)); err == nil {
			return
		}
	}
	t.Fatalf("daemon did not listen; logs:\n%s", logs.String())
}

func journalLines(t *testing.T, home string) []string {
	t.Helper()
	paths, err := filepath.Glob(filepath.Join(home, "sessions", "*", "journal.jsonl"))
	if err != nil || len(paths) != 1 {
		t.Fatalf("journals = %v, %v", paths, err)
	}
	data, err := os.ReadFile(paths[0])
	if err != nil {
		t.Fatal(err)
	}
	return strings.Split(strings.TrimSpace(string(data)), "\n")
}

func countLines(lines []string, parts ...string) int {
	count := 0
	for _, line := range lines {
		matched := true
		for _, part := range parts {
			matched = matched && strings.Contains(line, part)
		}
		if matched {
			count++
		}
	}
	return count
}

func shortDaemonHome(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "dalgona-home-*")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	return dir
}

func TestRunWorkflowExecsTwiceThroughTheDaemon(t *testing.T) {
	home, work := shortDaemonHome(t), t.TempDir()
	t.Setenv("HOME", t.TempDir())
	t.Setenv("DALGONA_HOME", home)
	config := "model = \"openai/gpt-test\"\napproval = \"yolo\"\nsandbox = \"off\"\n"
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
	workflow := "a = ctx.tools.exec(command = \"echo one-%d\" % args[\"n\"])\n" +
		"b = ctx.tools.exec(command = \"echo two\")\nprint(a)\nprint(b)\n{\"n\": args[\"n\"], \"label\": args[\"label\"]}\n"
	if err := os.WriteFile(filepath.Join(work, "flow.star"), []byte(workflow), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Chdir(work)
	startDaemon(t, home)

	var out, errOut bytes.Buffer
	code := run(t.Context(), []string{"run", "flow.star", "--arg", "n=2", "--arg", "label=plain text"}, stdio{in: strings.NewReader(""), out: &out, errOut: &errOut})
	if code != 0 {
		t.Fatalf("exit = %d, stdout = %q, stderr = %q", code, out.String(), errOut.String())
	}
	if !strings.Contains(out.String(), "one-2") || !strings.Contains(out.String(), "two") ||
		!strings.Contains(out.String(), `{"n":2,"label":"plain text"}`) {
		t.Fatalf("stdout = %q", out.String())
	}
	lines := journalLines(t, home)
	if got := countLines(lines, `"type":"tool.started"`, `"tool":"exec"`); got != 2 {
		t.Fatalf("exec starts = %d, journal:\n%s", got, strings.Join(lines, "\n"))
	}
	if got := countLines(lines, `"type":"script.completed"`, `"ok":true`); got != 1 {
		t.Fatalf("script.completed ok = %d, journal:\n%s", got, strings.Join(lines, "\n"))
	}
}

func TestRunRejectsMalformedArguments(t *testing.T) {
	t.Parallel()
	for _, args := range [][]string{{}, {"--arg", "a=1"}, {"flow.star", "--arg"}, {"flow.star", "--arg", "novalue"}, {"flow.star", "extra"}} {
		var out, errOut bytes.Buffer
		if code := runWorkflow(t.Context(), args, stdio{out: &out, errOut: &errOut}); code != 2 {
			t.Fatalf("args %q: exit = %d, stderr = %q", args, code, errOut.String())
		}
	}
}
