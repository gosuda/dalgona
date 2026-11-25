package rpcserver

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

type blockingCloseFiles struct {
	started  chan struct{}
	release  chan struct{}
	finished chan struct{}
	mu       sync.Mutex
	calls    int
}

func (b *blockingCloseFiles) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	return runner.WorkspaceResult{}, nil
}

func (b *blockingCloseFiles) Close() error {
	close(b.started)
	<-b.release
	b.mu.Lock()
	b.calls++
	b.mu.Unlock()
	close(b.finished)
	return nil
}

func (b *blockingCloseFiles) total() int {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.calls
}

type countingCloseFiles struct {
	mu    sync.Mutex
	calls int
}

func (c *countingCloseFiles) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	return runner.WorkspaceResult{}, nil
}

func (c *countingCloseFiles) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.calls++
	return nil
}

func (c *countingCloseFiles) total() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.calls
}

func admitCloseFixtures(t *testing.T, table *workspaceTable, blocked *blockingCloseFiles, other *countingCloseFiles, now time.Time) {
	t.Helper()
	if err := table.admit(&workspaceHandle{id: "blocked", rights: rightRead | rightWrite, used: now, reads: map[uint64]context.CancelFunc{}, files: blocked}); err != nil {
		t.Fatal(err)
	}
	if err := table.admit(&workspaceHandle{id: "other", rights: rightRead | rightWrite, used: now, reads: map[uint64]context.CancelFunc{}, files: other}); err != nil {
		t.Fatal(err)
	}
}

func startBlockedClose(table *workspaceTable) <-chan string {
	result := make(chan string, 1)
	go func() {
		status, err := table.close("blocked")
		if err != nil {
			result <- "error"
			return
		}
		result <- status
	}()
	return result
}

func requireTableLockFreeDuringClose(t *testing.T, table *workspaceTable, blocked *blockingCloseFiles) {
	t.Helper()
	<-blocked.started
	if !table.mu.TryLock() {
		close(blocked.release)
		t.Fatal("workspace close holds table lock while closing files")
	}
	table.mu.Unlock()
}

func useOtherWorkspaceWhileCloseBlocked(t *testing.T, table *workspaceTable, blocked *blockingCloseFiles) {
	t.Helper()
	use, err := table.acquire(context.Background(), "other", rightRead, false)
	if err != nil {
		close(blocked.release)
		t.Fatal(err)
	}
	use.release()
	close(blocked.release)
}

func requireBlockedCloseResult(t *testing.T, result <-chan string, blocked *blockingCloseFiles, other *countingCloseFiles) {
	t.Helper()
	status := <-result
	select {
	case <-blocked.finished:
	default:
		t.Fatal("close returned before files closed")
	}
	if status != "closed" {
		t.Fatalf("close status %q, want closed", status)
	}
	if blocked.total() != 1 {
		t.Fatalf("blocked close count %d, want 1", blocked.total())
	}
	if other.total() != 0 {
		t.Fatalf("other close count %d, want 0", other.total())
	}
}

func requireRepeatCloses(t *testing.T, table *workspaceTable, other *countingCloseFiles) {
	t.Helper()
	status, err := table.close("blocked")
	if err != nil || status != "notFound" {
		t.Fatalf("repeat close = %q, %v, want notFound", status, err)
	}
	status, err = table.close("other")
	if err != nil || status != "closed" {
		t.Fatalf("other close = %q, %v, want closed", status, err)
	}
	if other.total() != 1 {
		t.Fatalf("other close count %d, want 1", other.total())
	}
}

func TestWorkspaceCloseOutsideTableLock(t *testing.T) {
	table := &workspaceTable{}
	blocked := &blockingCloseFiles{started: make(chan struct{}), release: make(chan struct{}), finished: make(chan struct{})}
	other := &countingCloseFiles{}
	now := time.Now()
	admitCloseFixtures(t, table, blocked, other, now)
	result := startBlockedClose(table)
	requireTableLockFreeDuringClose(t, table, blocked)
	useOtherWorkspaceWhileCloseBlocked(t, table, blocked)
	requireBlockedCloseResult(t, result, blocked, other)
	requireRepeatCloses(t, table, other)
}
