package tui

import (
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestLoginCompletedNotice(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		params string
		notice string
		failed bool
	}{
		{
			name:   "success",
			params: `{"success":true,"provider":"openai"}`,
			notice: "Signed in to openai. Run /model to choose a model",
		},
		{
			name:   "failure",
			params: `{"success":false,"error":"openai did not accept the API key; it was not saved","provider":"openai"}`,
			notice: "Login failed: openai did not accept the API key; it was not saved; run /login again",
			failed: true,
		},
		{
			name:   "failure without error text",
			params: `{"success":false,"provider":"openai"}`,
			notice: "Login failed: unknown error; run /login again",
			failed: true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := newModel(nil, Options{}, nil)
			m, _ = drive(t, m, notificationMsg{method: "account/login/completed", params: []byte(tc.params)})
			if m.notice != tc.notice {
				t.Fatalf("notice = %q, want %q", m.notice, tc.notice)
			}
			if m.noticeError != tc.failed {
				t.Fatalf("noticeError = %v, want %v", m.noticeError, tc.failed)
			}
		})
	}
}

func TestLoginNoticeWaitsForProvider(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.applyLogin(&rpc.LoginAccountResponse{AuthURL: "https://example.test/auth", UserCode: "ABCD-1234"})
	want := "Open https://example.test/auth to sign in; enter code ABCD-1234; waiting for the provider to confirm"
	if m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestLoginResponseWithoutURLShowsNothing(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.applyLogin(&rpc.LoginAccountResponse{Type: "apiKey"})
	if m.notice != "" {
		t.Fatalf("notice = %q, want no notice; the outcome arrives as account/login/completed", m.notice)
	}
}
