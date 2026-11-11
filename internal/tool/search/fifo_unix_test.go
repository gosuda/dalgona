//go:build unix

package search

import (
	"path/filepath"
	"syscall"
	"testing"
	"time"
)

func TestSearchSkipsFIFO(t *testing.T) {
	cwd := t.TempDir()
	writeSearchFixture(t, cwd, "ok.txt", "nothing to find here\n")
	if err := syscall.Mkfifo(filepath.Join(cwd, "pipe"), 0o644); err != nil {
		t.Fatal(err)
	}
	done := make(chan outcome, 1)
	failed := make(chan error, 1)
	go func() {
		out, err := execute(cwd, args{Mode: "grep", Pattern: "nothing", Limit: 100})
		if err != nil {
			failed <- err
			return
		}
		done <- out
	}()
	select {
	case err := <-failed:
		t.Fatalf("search over FIFO workspace failed: %v", err)
	case out := <-done:
		if got := out.Text; got != "ok.txt:1: nothing to find here" {
			t.Fatalf("search over FIFO workspace = %q, want ok.txt hit", got)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("search blocked on FIFO after 30s")
	}
}
