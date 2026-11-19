package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func loginURLWithHyphens(n int) string {
	const head = "https://auth.example.test/oauth/authorize?"
	var b strings.Builder
	b.WriteString(head)
	for i := 0; b.Len() < n; i++ {
		b.WriteString("state-")
		b.WriteByte(byte('a' + i%26))
		b.WriteString("1-x&")
	}
	return b.String()[:n]
}

func waitingLoginPanel(t *testing.T, width, height int, response *rpc.LoginAccountResponse) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m.executeSlash("login", "chatgpt")
	m, _ = drive(t, m, resultMsg{op: "providers", value: providerList()})
	if m.login == nil {
		t.Fatal("login panel did not open")
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if m.login.phase != loginWaiting {
		t.Fatalf("login phase = %v, want the waiting phase", m.login.phase)
	}
	m, _ = drive(t, m, resultMsg{op: "login", value: response})
	return m
}

func loginPanelText(t *testing.T, m model) []string {
	t.Helper()
	var rows []string
	for _, line := range m.dockLines() {
		plain := ansi.Strip(line)
		if rest, ok := strings.CutPrefix(plain, "│ "); ok {
			rows = append(rows, strings.TrimPrefix(rest, "  "))
		}
	}
	if len(rows) == 0 {
		t.Fatalf("dock has no panel body rows:\n%s", strings.Join(m.dockLines(), "\n"))
	}
	return rows
}

func urlEntries(m model, url string) []string {
	var found []string
	for _, entry := range m.out.entries {
		if strings.Contains(strings.ReplaceAll(entry.text, "\n", ""), url) {
			found = append(found, entry.text)
		}
	}
	return found
}

func joinedPanelRows(t *testing.T, rows []string) string {
	t.Helper()
	var joined strings.Builder
	for _, row := range rows {
		row = strings.TrimRight(row, " ")
		if strings.HasSuffix(row, "...") {
			t.Fatalf("panel row %q is cut with an ellipsis", row)
		}
		joined.WriteString(row)
	}
	return joined.String()
}

func assertOneURLEntry(t *testing.T, m model, url string) {
	t.Helper()
	found := urlEntries(m, url)
	if len(found) != 1 {
		t.Fatalf("history holds %d entries with the URL, want 1: %q", len(found), found)
	}
	if want := "Sign-in URL: " + url; found[0] != want {
		t.Fatalf("history entry = %q, want %q", found[0], want)
	}
}

func TestLoginURLShownWhole(t *testing.T) {
	t.Parallel()
	url := loginURLWithHyphens(180)
	if len(url) != 180 {
		t.Fatalf("test url has %d bytes, want 180", len(url))
	}
	m := waitingLoginPanel(t, 60, 30, &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: url})
	assertFrame(t, m.dockLines(), 60, 30)
	rows := loginPanelText(t, m)
	if got := joinedPanelRows(t, rows); got != url {
		t.Fatalf("panel body rows joined = %q\nwant the sign-in URL %q", got, url)
	}
	if want := (len(url) + 53) / 54; len(rows) != want {
		t.Fatalf("panel holds %d body rows, want %d rows of 54 cells", len(rows), want)
	}
	assertOneURLEntry(t, m, url)
	if strings.Contains(m.notice, url) {
		t.Fatalf("notice %q repeats the URL the panel shows", m.notice)
	}
}

func TestLoginURLShownWholeKeepsTheCodeAndTheWaitingState(t *testing.T) {
	t.Parallel()
	url := loginURLWithHyphens(130)
	m := waitingLoginPanel(t, 60, 30, &rpc.LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: "login-2", VerificationURL: url, UserCode: "ABCD-1234"})
	rows := loginPanelText(t, m)
	if !strings.Contains(rows[0], "ABCD-1234") {
		t.Fatalf("first panel row = %q, want the user code", rows[0])
	}
	if got := joinedPanelRows(t, rows[1:]); got != url {
		t.Fatalf("rows after the code joined = %q, want %q", got, url)
	}
	frame := ansi.Strip(strings.Join(m.dockLines(), "\n"))
	assertContains(t, frame, "Esc cancel the sign-in", "waiting for the provider")
	assertOneURLEntry(t, m, url)
}

func TestLoginURLShownWholeStripsEscapeSequences(t *testing.T) {
	t.Parallel()
	hostile := "https://auth.example.test/a\x1b]0;pwn\x07" + loginURLWithHyphens(120)[26:] + "\u202e"
	m := waitingLoginPanel(t, 60, 30, &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-3", AuthURL: hostile, UserCode: "AB\x1b]0;x\x07CD"})
	shown := strings.Join(m.dockLines(), "\n") + "\n" + committedText(m)
	if strings.Contains(shown, "\x1b]") || strings.Contains(shown, "\u202e") || strings.Contains(shown, "\x07") {
		t.Fatalf("screen or history kept untrusted control text:\n%q", shown)
	}
	assertContains(t, ansi.Strip(shown), "https://auth.example.test/a", "ABCD")
}

func TestLoginURLReissuedForANewSignInIsCommittedAgain(t *testing.T) {
	t.Parallel()
	first := "https://auth.example.test/first"
	second := "https://auth.example.test/second"
	m := waitingLoginPanel(t, 60, 30, &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: first})
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m.openLogin("chatgpt")
	m, _ = drive(t, m, resultMsg{op: "providers", value: providerList()})
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, resultMsg{op: "login", value: &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-2", AuthURL: second}})
	if len(urlEntries(m, first)) != 1 || len(urlEntries(m, second)) != 1 {
		t.Fatalf("history entries = %q, want one for each sign-in", committedText(m))
	}
}
