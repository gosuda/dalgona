package agent

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
)

func TestReproMaskFailureFailsClosed(t *testing.T) {
	authHome := t.TempDir()
	store, err := auth.NewWithPatterns(authHome, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) {
		h.cfg.Auth = store
	})
	thread := h.start()
	breakAuthHome(t, authHome)
	submitErr := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "please handle token-abcdefgh now"})
	if submitErr == nil {
		journal := h.journalBytes(thread.ID())
		if strings.Contains(journal, "secret masking failed") {
			t.Fatalf("submit succeeded and journaled sentinel, want submit error")
		}
		t.Fatalf("submit succeeded, want masking failure error")
	}
	if !strings.Contains(submitErr.Error(), "secret masking failed") {
		t.Fatalf("submit error = %q, want masking failure", submitErr.Error())
	}
	journal := h.journalBytes(thread.ID())
	if strings.Contains(journal, "secret masking failed") {
		t.Fatalf("journal contains sentinel after failed submit")
	}
	if strings.Contains(journal, "token-abcdefgh") {
		t.Fatalf("journal contains raw secret after failed submit")
	}
}

func TestProjectInstructionsAreMaskedBeforeStore(t *testing.T) {
	const secret = "token-abcdefgh1234"
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) {
		h.cfg.Auth = store
	})
	err = os.WriteFile(filepath.Join(h.cwd, "AGENTS.md"), []byte("use "+secret+" with care\n"), 0o644)
	if err != nil {
		t.Fatal(err)
	}
	h.model.push(text("done"))
	thread := h.start()
	h.submit(thread, "hello")
	h.waitIdle(thread)
	stored := instructionsBlob(t, h, thread.ID())
	if strings.Contains(string(stored), secret) {
		t.Fatal("instructions blob contains the raw secret")
	}
	if !strings.Contains(string(stored), "{{secret:pattern0:") {
		t.Fatal("instructions blob lacks the masked placeholder")
	}
	assertMaskedRequests(t, h, secret)
}

func instructionsBlob(t *testing.T, h *harness, id string) []byte {
	t.Helper()
	for _, item := range h.events(id) {
		if created, ok := item.Event.(event.SessionCreated); ok && created.InstructionsBlob != nil {
			stored, err := h.journal.Blob(created.InstructionsBlob.Blob)
			if err != nil {
				t.Fatal(err)
			}
			return stored
		}
	}
	t.Fatal("session.created lacks an instructions blob")
	return nil
}

func assertMaskedRequests(t *testing.T, h *harness, secret string) {
	t.Helper()
	for i := range h.model.count() {
		raw := h.model.raw(i)
		if strings.Contains(raw, secret) {
			t.Fatalf("model request %d contains the secret", i)
		}
		if !strings.Contains(raw, "{{secret:pattern0:") {
			t.Fatalf("model request %d lacks the instruction placeholder", i)
		}
	}
}

func breakAuthHome(t *testing.T, dir string) {
	t.Helper()
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(dir, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = os.Remove(dir)
		_ = os.Mkdir(dir, 0o700)
	})
}
