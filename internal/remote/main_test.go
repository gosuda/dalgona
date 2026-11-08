package remote

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"runtime/debug"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 {
		switch os.Args[1] {
		case "__shim":
			os.Exit(runner.ShimMain(os.Args[2:]))
		case "exec-server":
			os.Exit(runner.ExecServerMain(os.Args[2:], os.Stdin, os.Stdout))
		case "--version":
			_, _ = fmt.Fprintf(os.Stdout, "dalgona %s\n", testBinaryVersion())
			os.Exit(0)
		}
	}
	os.Exit(m.Run())
}

func testBinaryVersion() string {
	info, ok := debug.ReadBuildInfo()
	if !ok || info.Main.Version == "" {
		return "(devel)"
	}
	return info.Main.Version
}

type journalRecorder struct {
	store  *journal.Store
	closed map[string]bool
}

func (j *journalRecorder) RecordRunnerBound(ctx context.Context, threadID string, b event.RunnerBound) error {
	if j.closed[threadID] {
		return fmt.Errorf("agent: thread %s is not open", threadID)
	}
	return appendEvents(ctx, j.store, threadID, b)
}

func appendEvents(ctx context.Context, store *journal.Store, threadID string, events ...event.Event) error {
	raws := make([][]byte, 0, len(events))
	for _, e := range events {
		raw, err := event.Encode(e)
		if err != nil {
			return err
		}
		raws = append(raws, raw)
	}
	_, err := store.Append(ctx, threadID, raws...)
	return err
}

func openJournal(t *testing.T) *journal.Store {
	t.Helper()
	store, err := journal.Open(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	return store
}

func startThread(t *testing.T, store *journal.Store, id, cwd, runnerName string) {
	t.Helper()
	appendCreated(t, store, event.SessionCreated{ID: id, Cwd: cwd, Runner: runnerName})
}

func startFork(t *testing.T, store *journal.Store, id, parent string) {
	t.Helper()
	appendCreated(t, store, event.SessionCreated{ID: id, ForkOf: parent, Cwd: t.TempDir(), Runner: "box"})
}

func appendCreated(t *testing.T, store *journal.Store, created event.SessionCreated) {
	t.Helper()
	created.Model, created.Approval, created.Sandbox = "test/model", "auto", "workspace-write"
	created.CreatedAt = time.Unix(1, 0).UTC()
	if err := appendEvents(t.Context(), store, created.ID, created); err != nil {
		t.Fatal(err)
	}
}

func bindings(t *testing.T, store *journal.Store, threadID string) []event.RunnerBound {
	t.Helper()
	var found []event.RunnerBound
	err := store.Replay(threadID, func(_ uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		if err != nil {
			return err
		}
		if bound, ok := decoded.(event.RunnerBound); ok {
			found = append(found, bound)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return found
}

type logCapture struct {
	mu       sync.Mutex
	messages []string
}

func (c *logCapture) Enabled(context.Context, slog.Level) bool { return true }

func (c *logCapture) Handle(_ context.Context, record slog.Record) error {
	c.mu.Lock()
	c.messages = append(c.messages, record.Message)
	c.mu.Unlock()
	return nil
}

func (c *logCapture) WithAttrs([]slog.Attr) slog.Handler { return c }
func (c *logCapture) WithGroup(string) slog.Handler      { return c }

func (c *logCapture) has(message string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Contains(c.messages, message)
}
