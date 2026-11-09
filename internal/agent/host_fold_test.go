package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/journal"
)

func foldJournal(t *testing.T, h *harness, id string) event.State {
	t.Helper()
	state := event.State{}
	if err := h.journal.Replay(id, func(seq uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		if err != nil {
			return err
		}
		next, err := event.Fold(state, seq, decoded)
		if err != nil {
			return err
		}
		state = next
		return nil
	}); err != nil {
		t.Fatalf("agent: replay: %v", err)
	}
	return state
}

func TestThreadInfoAgreesWithFold(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	if err := thread.SetName(t.Context(), "listed"); err != nil {
		t.Fatalf("agent: set name: %v", err)
	}
	id := thread.ID()
	heads, err := h.journal.Sessions()
	if err != nil {
		t.Fatalf("agent: sessions: %v", err)
	}
	var head uint64
	for _, item := range heads {
		if item.ID == id {
			head = item.HeadSeq
		}
	}
	info, err := h.host.sessionInfo(journal.SessionHead{ID: id, HeadSeq: head})
	if err != nil {
		t.Fatalf("agent: sessionInfo: %v", err)
	}
	state := foldJournal(t, h, id)
	if info.Name != state.Name || info.Archived != state.Archived {
		t.Fatalf("agent: listing %+v disagrees with fold %+v", info, state)
	}
}
