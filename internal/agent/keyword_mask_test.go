package agent

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
)

func TestKeywordNoticeIsMaskedBeforeStore(t *testing.T) {
	t.Parallel()
	const secret = "token-abcdefgh1234"
	h := reviewHarness(t, t.TempDir(), "check "+secret+" before merge")
	h.model.push(text("done"))
	thread := h.start()
	h.submit(thread, "please review this change")
	h.waitIdle(thread)
	if strings.Contains(h.journalBytes(thread.ID()), secret) {
		t.Fatal("journal bytes contain the raw secret")
	}
	notice := storedReviewNotice(t, h, thread.ID())
	if strings.Contains(notice, secret) {
		t.Fatal("stored notice contains the raw secret")
	}
	if !strings.Contains(notice, "{{secret:pattern0:") {
		t.Fatalf("stored notice = %q, want a masked placeholder", notice)
	}
	assertNoSecretInRequests(t, h, secret)
}

func reviewHarness(t *testing.T, home, notice string) *harness {
	t.Helper()
	store, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	return newHarness(t, func(h *harness) {
		h.cfg.Auth = store
		h.cfg.Config.Keywords = map[string]config.KeywordSpec{
			"review": {Text: notice, Enabled: true},
		}
	})
}

func storedReviewNotice(t *testing.T, h *harness, id string) string {
	t.Helper()
	for _, item := range h.events(id) {
		message, ok := item.Event.(event.UserMessage)
		if !ok {
			continue
		}
		for _, notice := range message.Notices {
			if notice.Word == "review" {
				return notice.Text
			}
		}
	}
	t.Fatal("no review notice in journal")
	return ""
}

func assertNoSecretInRequests(t *testing.T, h *harness, secret string) {
	t.Helper()
	for i := range h.model.count() {
		if strings.Contains(h.model.raw(i), secret) {
			t.Fatalf("model request %d contains the secret", i)
		}
	}
}

func TestKeywordNoticeMaskFailureFailsSubmit(t *testing.T) {
	t.Parallel()
	authHome := t.TempDir()
	h := reviewHarness(t, authHome, "check token-abcdefgh1234 before merge")
	thread := h.start()
	breakAuthHome(t, authHome)
	err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "please review this change"})
	if err == nil || !strings.Contains(err.Error(), "secret masking failed") {
		t.Fatalf("submit error = %v, want masking failure", err)
	}
	if strings.Contains(h.journalBytes(thread.ID()), "token-abcdefgh1234") {
		t.Fatal("journal contains raw secret after failed submit")
	}
}
