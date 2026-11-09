package agent

import (
	"os"
	"strings"
	"testing"

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
