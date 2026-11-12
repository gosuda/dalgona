package agent

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

func TestAskQuestionTextIsMasked(t *testing.T) {
	t.Parallel()
	const secret = "token-abcdefgh1234"
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) { h.cfg.Auth = store })
	thread := h.start()
	updates := thread.Subscribe()
	questions := []asktool.Question{{
		Header: secret,
		Text:   "Which key?",
		Options: []asktool.Option{
			{Label: secret, Description: "the key"},
			{Label: "none", Description: "no key"},
		},
	}}
	results := askAsync(t.Context(), thread, "call-1", questions)
	prompt := nextQuestionUpdate(t, updates, false)
	if strings.Contains(prompt.Questions[0].Header, secret) || !strings.Contains(prompt.Questions[0].Header, "{{secret:") {
		t.Fatalf("header = %q, want only a placeholder", prompt.Questions[0].Header)
	}
	if label := prompt.Questions[0].Options[0].Label; strings.Contains(label, secret) || !strings.Contains(label, "{{secret:") {
		t.Fatalf("label = %q, want only a placeholder", label)
	}
	if err := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	waitQuestionResult(t, results)
}
