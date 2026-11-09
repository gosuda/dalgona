package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
)

func sharedDalHost(t *testing.T, h *harness, home string) (*Host, *journal.Store) {
	t.Helper()
	store, err := journal.OpenShared(home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	cfg := h.cfg
	cfg.Journal = store
	cfg.Product, cfg.Config.Product, cfg.Config.Home = config.ProductDal, config.ProductDal, home
	host, err := NewHost(cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = host.Close() })
	return host, store
}

func completedTurn(t *testing.T, store *journal.Store, id string) bool {
	t.Helper()
	completed := false
	err := store.Replay(id, func(_ uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		if done, ok := decoded.(event.TurnCompleted); ok && done.Status == statusCompleted {
			completed = true
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return completed
}

func closeThread(t *testing.T, thread *Thread) {
	t.Helper()
	if err := thread.Close(); err != nil {
		t.Fatal(err)
	}
}

func savedSession(t *testing.T, host *Host) string {
	t.Helper()
	created, err := host.Start(t.Context(), ThreadSpec{})
	if err != nil {
		t.Fatal(err)
	}
	closeThread(t, created)
	return created.ID()
}

func resumeOrFail(t *testing.T, host *Host, id string) *Thread {
	t.Helper()
	thread, err := host.Resume(t.Context(), id)
	if err != nil {
		t.Fatalf("resume %s: %v", id, err)
	}
	return thread
}

func startOrFail(t *testing.T, host *Host) *Thread {
	t.Helper()
	thread, err := host.Start(t.Context(), ThreadSpec{})
	if err != nil {
		t.Fatal(err)
	}
	return thread
}

func TestSharedHomeDalHostsOwnSessionsExclusively(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	home := t.TempDir()
	first, firstStore := sharedDalHost(t, h, home)
	second, secondStore := sharedDalHost(t, h, home)

	id := savedSession(t, first)
	resumed := resumeOrFail(t, first, id)
	if _, err := second.Resume(t.Context(), id); err == nil || err.Error() != "another dal process owns session "+id {
		t.Fatalf("second resume error = %v", err)
	}
	own := startOrFail(t, second)

	h.submit(resumed, "first host turn")
	h.submit(own, "second host turn")
	h.waitIdle(resumed)
	h.waitIdle(own)
	firstDone, secondDone, crossRead := completedTurn(t, firstStore, id), completedTurn(t, secondStore, own.ID()), completedTurn(t, secondStore, id)
	if !firstDone || !secondDone || !crossRead {
		t.Fatalf("completed turns: first %v, second %v, first read by second %v", firstDone, secondDone, crossRead)
	}

	closeThread(t, resumed)
	closeThread(t, resumeOrFail(t, second, id))
}
