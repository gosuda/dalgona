package runner

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

func workspaceLocal(t *testing.T) (*Local, string) {
	t.Helper()
	root := t.TempDir()
	local, err := NewLocal(root)
	if err != nil {
		t.Fatal(err)
	}
	return local, root
}

func writeWorkspaceFile(t *testing.T, root, rel, data string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(root, rel), []byte(data), 0o644); err != nil {
		t.Fatal(err)
	}
}

func workspaceRevision(t *testing.T, local *Local, root, rel string) string {
	t.Helper()
	result, err := local.Workspace(context.Background(), WorkspaceRequest{Op: WorkspaceStat, Root: root, Path: rel})
	if err != nil {
		t.Fatal(err)
	}
	return result.Entry.Revision
}

func TestRenameNoReplaceRefusesExistingDestination(t *testing.T) {
	local, root := workspaceLocal(t)
	writeWorkspaceFile(t, root, "a.txt", "alpha\n")
	writeWorkspaceFile(t, root, "b.txt", "beta\n")
	revision := workspaceRevision(t, local, root, "a.txt")
	_, err := local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceRename, Root: root, Path: "a.txt", To: "b.txt", Expected: revision,
	})
	if !errors.Is(err, ErrWorkspaceExists) {
		t.Fatalf("rename onto an existing file = %v, want ErrWorkspaceExists", err)
	}
	if data, err := os.ReadFile(filepath.Join(root, "b.txt")); err != nil || string(data) != "beta\n" {
		t.Fatalf("destination = %q, %v; want the original bytes kept", data, err)
	}
}

func TestRenameNoReplaceMovesMissingDestination(t *testing.T) {
	local, root := workspaceLocal(t)
	writeWorkspaceFile(t, root, "a.txt", "alpha\n")
	revision := workspaceRevision(t, local, root, "a.txt")
	result, err := local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceRename, Root: root, Path: "a.txt", To: "c.txt", Expected: revision,
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.Entry.Name != "c.txt" {
		t.Fatalf("renamed entry = %q, want c.txt", result.Entry.Name)
	}
	if _, err := os.Lstat(filepath.Join(root, "a.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("source still exists: %v", err)
	}
	if data, err := os.ReadFile(filepath.Join(root, "c.txt")); err != nil || string(data) != "alpha\n" {
		t.Fatalf("destination = %q, %v; want the moved bytes", data, err)
	}
}

func TestRenameNoReplaceLetsExactlyOneWinnerThrough(t *testing.T) {
	const racers = 8
	for range 50 {
		local, root := workspaceLocal(t)
		for i := range racers {
			writeWorkspaceFile(t, root, fmt.Sprintf("src%d.txt", i), "payload\n")
		}
		revisions := make([]string, racers)
		for i := range racers {
			revisions[i] = workspaceRevision(t, local, root, fmt.Sprintf("src%d.txt", i))
		}
		winners := raceRename(t, local, root, revisions)
		if len(winners) != 1 {
			t.Fatalf("winners = %v, want exactly one atomic rename", winners)
		}
	}
}

func raceRename(t *testing.T, local *Local, root string, revisions []string) []int {
	t.Helper()
	work := &raceWork{group: &sync.WaitGroup{}, local: local, root: root, start: make(chan struct{}), won: make(chan int, len(revisions))}
	for i := range revisions {
		work.group.Add(1)
		go renameRacer(t, work, i, revisions[i])
	}
	close(work.start)
	work.group.Wait()
	close(work.won)
	var winners []int
	for w := range work.won {
		winners = append(winners, w)
	}
	return winners
}

type raceWork struct {
	group *sync.WaitGroup
	local *Local
	root  string
	start chan struct{}
	won   chan int
}

func renameRacer(t *testing.T, work *raceWork, i int, revision string) {
	defer work.group.Done()
	<-work.start
	_, err := work.local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceRename, Root: work.root,
		Path:     fmt.Sprintf("src%d.txt", i),
		To:       "winner.txt",
		Expected: revision,
	})
	if err == nil {
		work.won <- i
	} else if !errors.Is(err, ErrWorkspaceExists) {
		t.Errorf("racer %d error = %v, want nil or ErrWorkspaceExists", i, err)
	}
}

func TestRenameNoReplaceCoversDirectories(t *testing.T) {
	local, root := workspaceLocal(t)
	if err := os.MkdirAll(filepath.Join(root, "src", "nested"), 0o755); err != nil {
		t.Fatal(err)
	}
	writeWorkspaceFile(t, root, "src/nested/file.txt", "deep\n")
	if err := os.MkdirAll(filepath.Join(root, "taken"), 0o755); err != nil {
		t.Fatal(err)
	}
	revision := workspaceRevision(t, local, root, "src")
	_, err := local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceRename, Root: root, Path: "src", To: "taken", Expected: revision,
	})
	if !errors.Is(err, ErrWorkspaceExists) {
		t.Fatalf("directory rename onto an existing directory = %v, want ErrWorkspaceExists", err)
	}
	if _, err := local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceRename, Root: root, Path: "src", To: "moved", Expected: revision,
	}); err != nil {
		t.Fatal(err)
	}
	if data, err := os.ReadFile(filepath.Join(root, "moved", "nested", "file.txt")); err != nil || string(data) != "deep\n" {
		t.Fatalf("moved tree = %q, %v; want the directory contents kept", data, err)
	}
}

func TestPublishAbsentKeepsAnExistingDestination(t *testing.T) {
	local, root := workspaceLocal(t)
	writeWorkspaceFile(t, root, "target.txt", "original\n")
	stage, err := local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspaceStage, Root: root, Path: "target.txt", Data: []byte("staged\n"),
	})
	if err != nil {
		t.Fatal(err)
	}
	_, err = local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspacePublish, Root: root, Path: "target.txt",
		Staging: stage.Staging, Expected: WorkspaceAbsent,
	})
	if stale, ok := errors.AsType[*StaleRevisionError](err); !ok {
		t.Fatalf("publish onto an existing file = %v, want a stale revision conflict", err)
	} else if stale.Current == "" {
		t.Fatal("stale revision conflict carries no current revision")
	}
	if data, err := os.ReadFile(filepath.Join(root, "target.txt")); err != nil || string(data) != "original\n" {
		t.Fatalf("destination = %q, %v; want the original bytes kept", data, err)
	}
}

func TestListOnAFifoRefusesTheWrongKind(t *testing.T) {
	local, root := workspaceLocal(t)
	if err := makeFifo(filepath.Join(root, "pipe")); err != nil {
		t.Skipf("fifo unavailable: %v", err)
	}
	_, err := local.Workspace(context.Background(), WorkspaceRequest{Op: WorkspaceList, Root: root, Path: "pipe"})
	if !errors.Is(err, ErrWorkspaceKind) {
		t.Fatalf("list on a fifo = %v, want ErrWorkspaceKind", err)
	}
}

func TestPublishAbsentLetsExactlyOneWinnerThrough(t *testing.T) {
	const racers = 8
	for range 50 {
		local, root := workspaceLocal(t)
		staged := make([]string, racers)
		for i := range racers {
			result, err := local.Workspace(context.Background(), WorkspaceRequest{
				Op: WorkspaceStage, Root: root, Path: "target.txt", Data: []byte("staged\n"),
			})
			if err != nil {
				t.Fatal(err)
			}
			staged[i] = result.Staging
		}
		winners := racePublish(t, local, root, staged)
		if len(winners) != 1 {
			t.Fatalf("winners = %v, want exactly one atomic publish", winners)
		}
	}
}

func racePublish(t *testing.T, local *Local, root string, staged []string) []int {
	t.Helper()
	work := &raceWork{group: &sync.WaitGroup{}, local: local, root: root, start: make(chan struct{}), won: make(chan int, len(staged))}
	for i := range staged {
		work.group.Add(1)
		go publishRacer(t, work, i, staged[i])
	}
	close(work.start)
	work.group.Wait()
	close(work.won)
	var winners []int
	for w := range work.won {
		winners = append(winners, w)
	}
	return winners
}

func publishRacer(t *testing.T, work *raceWork, i int, staging string) {
	defer work.group.Done()
	<-work.start
	_, err := work.local.Workspace(context.Background(), WorkspaceRequest{
		Op: WorkspacePublish, Root: work.root, Path: "target.txt",
		Staging: staging, Expected: WorkspaceAbsent,
	})
	if err == nil {
		work.won <- i
	} else if stale, ok := errors.AsType[*StaleRevisionError](err); !ok || stale.Current == "" {
		t.Errorf("racer %d error = %v, want a stale revision conflict", i, err)
	}
}
