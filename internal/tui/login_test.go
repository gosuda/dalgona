package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

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

func providerList() *rpc.AccountProvidersResponse {
	return &rpc.AccountProvidersResponse{Providers: []rpc.AccountProvider{
		{ID: "anthropic", Login: "apiKey"},
		{ID: "chatgpt", Login: "browser"},
		{ID: "openai", Login: "apiKey", SignedIn: true},
		{ID: "cursor", Login: "terminal"},
	}}
}

func openLoginPanel(t *testing.T, client caller, argument string) model {
	t.Helper()
	m := newTestModel(client, Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.executeSlash("login", argument)
	m, _ = drive(t, m, resultMsg{op: "providers", value: providerList()})
	if m.login == nil {
		t.Fatal("login panel did not open")
	}
	return m
}

func TestLoginPanelApiKeyFlow(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openLoginPanel(t, caller, "openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = typeText(t, m, "sk-live-test")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runPickCommand(t, m, command)
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.AccountLoginStartParams)
	want := rpc.AccountLoginStartParams{Type: "apiKey", Provider: "openai", APIKey: "sk-live-test"}
	if last.method != "account/login/start" || !ok || params != want {
		t.Fatalf("last call = %s %#v, want account/login/start %#v", last.method, last.params, want)
	}
	if m.login != nil {
		t.Fatal("login panel stayed open after the provider accepted the key")
	}
	m, _ = drive(t, m, notificationMsg{method: "account/login/completed", params: []byte(`{"success":true,"provider":"openai"}`)})
	assertShown(t, m, "Signed in to openai. Run /model to choose a model")
}

func TestLoginPanelOAuthCompletes(t *testing.T) {
	t.Parallel()
	m := openLoginPanel(t, new(fakeCaller), "chatgpt")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, resultMsg{op: "login", value: &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: "https://auth.example/start"}})
	assertContains(t, m.View().Content, "https://auth.example/start")
	m, _ = drive(t, m, notificationMsg{method: "account/login/completed", params: []byte(`{"success":true,"provider":"chatgpt"}`)})
	if m.login != nil {
		t.Fatal("login panel stayed open after the sign-in completed")
	}
	assertShown(t, m, "Signed in to chatgpt. Run /model to choose a model")
}

func TestLoginPanelCancelSendsCancel(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openLoginPanel(t, caller, "chatgpt")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, resultMsg{op: "login", value: &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: "https://auth.example/start"}})
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runPickCommand(t, m, command)
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.AccountLoginCancelParams)
	if last.method != "account/login/cancel" || !ok || params.LoginID != "login-1" {
		t.Fatalf("last call = %s %#v, want account/login/cancel for login-1", last.method, last.params)
	}
	if m.login != nil {
		t.Fatal("login panel stayed open after the cancel")
	}
}

func TestLoginEscDuringStartKeepsPanelUntilAnswer(t *testing.T) {
	t.Parallel()
	m := openLoginPanel(t, new(fakeCaller), "chatgpt")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.login == nil {
		t.Fatal("login panel closed before the start call answered, so the cancel has no panel to wait on")
	}
	assertContains(t, m.View().Content, "Cancelling sign-in")
}

func TestLoginEscDuringStartCancelsWhenStartAnswers(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openLoginPanel(t, caller, "chatgpt")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m, command := drive(t, m, resultMsg{op: "login", value: &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: "https://auth.example/start"}})
	m = runPickCommand(t, m, command)
	if len(caller.calls) != 1 {
		t.Fatalf("calls = %d, want one account/login/cancel", len(caller.calls))
	}
	last := caller.calls[0]
	params, ok := last.params.(rpc.AccountLoginCancelParams)
	if last.method != "account/login/cancel" || !ok || params.LoginID != "login-1" {
		t.Fatalf("call = %s %#v, want account/login/cancel for login-1", last.method, last.params)
	}
	if strings.Contains(m.notice, "auth.example") {
		t.Fatalf("notice = %q, want no sign-in link after the cancel", m.notice)
	}
	if m.login != nil {
		t.Fatal("login panel stayed open after the cancel")
	}
}

func TestLoginKeyNeverRendered(t *testing.T) {
	t.Parallel()
	m := openLoginPanel(t, new(fakeCaller), "openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = typeText(t, m, "sk-1234")
	view := m.View().Content
	assertContains(t, view, "API key: "+strings.Repeat("*", 7))
	if strings.Contains(view, "sk-1234") {
		t.Fatalf("view shows the typed key:\n%s", view)
	}
}

func TestLoginPanelSteady(t *testing.T) {
	t.Parallel()
	m := openLoginPanel(t, new(fakeCaller), "")
	if got := len(m.dockLines()); got != 15 {
		t.Fatalf("dock has %d rows with the providers open, want 15", got)
	}
	down := make([]tea.KeyPressMsg, 50)
	for i := range down {
		down[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	m = assertSteadyDock(t, m, down)
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	typed := make([]tea.KeyPressMsg, 50)
	for i := range typed {
		typed[i] = press('k', 0, "k")
	}
	assertSteadyDock(t, m, typed)
}
