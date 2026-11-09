package agent

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type placeholderSeed struct {
	masked string
	pre    string
	post   string
}

func TestRecoverStrictUnmaskAbortsOnMissingPatternSecret(t *testing.T) {
	t.Parallel()
	patterns := []string{`token-[a-z0-9]{8,}`}
	authHome := t.TempDir()
	store, err := auth.NewWithPatterns(authHome, patterns)
	if err != nil {
		t.Fatal(err)
	}
	const secret = "token-abcdefgh"
	seed := placeholderSeed{pre: "prefix " + secret + " suffix\n", post: "changed\n"}
	seed.masked = store.Mask(seed.pre)
	if err := store.MaskError(); err != nil {
		t.Fatalf("mask error: %v", err)
	}
	if !strings.Contains(seed.masked, "{{secret:pattern0:") {
		t.Fatalf("pre not masked: %q", seed.masked)
	}
	h := newHarness(t, func(h *harness) {
		h.cfg.Auth = store
	})
	change := seededPlaceholderChange(t, h, authHome, patterns, seed)
	h.write("a.txt", seed.post)
	h.write("b.txt", "b-foreign\n")
	args := `{"changes":[{"path":"a.txt","old":"old","new":"new"},{"path":"b.txt","old":"b-old","new":"b-new"}]}`
	h.seed(prefix(openTurn("edit"), []event.Event{assistantCall("c1", "patch", args)}, approved("c1", "edit"),
		[]event.Event{startedCall("c1", "patch", "edit", args, change, h.fileChange("b.txt", "b-old\n", "b-new\n"))})...)
	host := h.newHost()
	_ = host.Recover(t.Context())
	thread, resumeErr := host.Resume(t.Context(), crashSession)
	if resumeErr != nil {
		wantRestoreError(t, resumeErr.Error())
		if got := h.read("a.txt"); got != seed.post {
			t.Fatalf("file = %q, want unchanged %q", got, seed.post)
		}
		return
	}
	h.waitIdle(thread)
	turn := lastTurn(h.events(crashSession))
	if turn.Status != statusFailed {
		t.Fatalf("turn status = %q, want failed", turn.Status)
	}
	wantRestoreError(t, turn.Error)
	if got := h.read("a.txt"); got != seed.post {
		t.Fatalf("file = %q, want unchanged %q", got, seed.post)
	}
	if got := h.read("a.txt"); strings.Contains(got, "{{secret:") {
		t.Fatalf("placeholder written to file: %q", got)
	}
}

func seededPlaceholderChange(t *testing.T, h *harness, authHome string, patterns []string, seed placeholderSeed) event.FileChange {
	t.Helper()
	sha, err := h.journal.PutBlob([]byte(seed.masked))
	if err != nil {
		t.Fatal(err)
	}
	change := h.fileChange("a.txt", seed.pre, seed.post)
	change.PreSHA = snapshot.Digest([]byte(seed.pre))
	change.PostSHA = snapshot.Digest([]byte(seed.post))
	change.PreBlob.Blob = sha
	change.PreBlob.Bytes = int64(len(seed.masked))
	_ = os.Remove(filepath.Join(authHome, "secrets.json"))
	reloaded, err := auth.NewWithPatterns(authHome, patterns)
	if err != nil {
		t.Fatal(err)
	}
	h.cfg.Auth = reloaded
	return change
}

func wantRestoreError(t *testing.T, message string) {
	t.Helper()
	if !strings.Contains(message, "cannot be restored") && !strings.Contains(message, "placeholder") {
		t.Fatalf("error = %q, want placeholder restore error", message)
	}
}
