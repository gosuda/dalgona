package agent

import (
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/journal"
)

func journalHead(t *testing.T, h *harness, id string) uint64 {
	t.Helper()
	heads, err := h.journal.Sessions()
	if err != nil {
		t.Fatalf("agent: sessions: %v", err)
	}
	for _, item := range heads {
		if item.ID == id {
			return item.HeadSeq
		}
	}
	t.Fatalf("agent: session %s missing from journal index", id)
	return 0
}

func corruptJournal(t *testing.T, h *harness, id string) {
	t.Helper()
	path := filepath.Join(h.home, "sessions", id, "journal.jsonl")
	data, readErr := os.ReadFile(path)
	if readErr != nil {
		t.Fatalf("agent: read journal: %v", readErr)
	}
	broken := append(slices.Clone(data), []byte("{\"type\":\"user.message\",broken\n")...)
	if writeErr := os.WriteFile(path, broken, 0o600); writeErr != nil {
		t.Fatalf("agent: corrupt journal: %v", writeErr)
	}
}

func TestSessionInfoKeepsUndecodableSession(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	id := thread.ID()
	head := journalHead(t, h, id)
	corruptJournal(t, h, id)
	info, err := h.host.sessionInfo(journal.SessionHead{ID: id, HeadSeq: head})
	if err != nil {
		t.Fatalf("agent: sessionInfo dropped undecodable session: %v", err)
	}
	if !info.Damaged {
		t.Fatalf("agent: undecodable session not marked damaged: %+v", info)
	}
	if info.ID != id {
		t.Fatalf("agent: session id = %s, want %s", info.ID, id)
	}
}

func TestSessionInfoPropagatesJournalIOError(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	id := thread.ID()
	head := journalHead(t, h, id)
	path := filepath.Join(h.home, "sessions", id, "journal.jsonl")
	if err := os.Remove(path); err != nil {
		t.Fatalf("agent: remove journal: %v", err)
	}
	if err := os.Mkdir(path, 0o755); err != nil {
		t.Fatalf("agent: replace journal with dir: %v", err)
	}
	if _, err := h.host.sessionInfo(journal.SessionHead{ID: id, HeadSeq: head}); err == nil {
		t.Fatal("agent: sessionInfo hid journal I/O error as damaged")
	}
}
