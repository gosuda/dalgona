package tui

import (
	"fmt"
	"slices"
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
	m, _ := drive(t, startingLoginPanel(t, width, height), resultMsg{op: "login", value: response})
	return m
}

func startingLoginPanel(t *testing.T, width, height int) model {
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

func assertOneURLEntry(t *testing.T, m model, url, code string) {
	t.Helper()
	found := urlEntries(m, url)
	if len(found) != 1 {
		t.Fatalf("history holds %d entries with the URL, want 1: %q", len(found), found)
	}
	row := "Sign-in URL: " + url
	want := row
	if code != "" {
		want += "\nUser code: " + code
	}
	if found[0] != want {
		t.Fatalf("history entry = %q, want %q", found[0], want)
	}
}

func assertPrintedRowsHideTheCode(t *testing.T, response *rpc.LoginAccountResponse) {
	t.Helper()
	m := rawUpdate(t, startingLoginPanel(t, 20, 6), resultMsg{op: "login", value: response})
	printed := strings.Join(m.out.pending, "")
	if !strings.Contains(printed, response.VerificationURL) || strings.Contains(printed, response.UserCode) {
		t.Fatalf("printed rows = %q, want the sign-in URL without the user code %q", m.out.pending, response.UserCode)
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
	assertOneURLEntry(t, m, url, "")
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
	assertOneURLEntry(t, m, url, "ABCD-1234")
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

func TestLoginURLFillsTheBodyWidth(t *testing.T) {
	t.Parallel()
	url := loginURLWithHyphens(180)
	m := waitingLoginPanel(t, 60, 30, &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: url})
	rows := loginPanelText(t, m)
	for i, row := range rows[:len(rows)-1] {
		if cells := ansi.StringWidth(row); cells != 54 {
			t.Fatalf("row %d holds %d cells, want the 56-cell body less its 2-cell marker column: %q", i, cells, row)
		}
	}
}

func isMoreRow(row string) bool {
	return strings.HasPrefix(row, "↓ ") && strings.Contains(row, "more lines")
}

func linkRows(rows []string) []string {
	var link []string
	for _, row := range rows {
		if !isMoreRow(row) {
			link = append(link, strings.TrimRight(row, " "))
		}
	}
	return link
}

func readLinkByScrolling(t *testing.T, m model, width, height int) (string, model) {
	t.Helper()
	assertFrame(t, m.dockLines(), width, height)
	first := linkRows(loginPanelText(t, m))
	link := slices.Clone(first)
	previous := first
	for range 200 {
		m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
		assertFrame(t, m.dockLines(), width, height)
		current := linkRows(loginPanelText(t, m))
		if slices.Equal(current, previous) {
			return strings.Join(link, ""), m
		}
		overlap := max(len(previous)-1, 0)
		if !slices.Equal(current[:overlap], previous[len(previous)-overlap:]) {
			t.Fatalf("one press of Down moved the rows from %q to %q, want a move of one row", previous, current)
		}
		link = append(link, current[overlap:]...)
		previous = current
	}
	t.Fatalf("the panel kept scrolling after 200 presses of Down")
	return "", m
}

func assertLoginURLScrolls(t *testing.T, width, height int, url string) {
	t.Helper()
	m := waitingLoginPanel(t, width, height, &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: url})
	top := linkRows(loginPanelText(t, m))
	if len(top) == 0 {
		t.Fatalf("panel shows no link row at %dx%d", width, height)
	}
	if rows := loginPanelText(t, m); width >= 40 && isMoreRow(rows[len(rows)-1]) {
		assertContains(t, rows[len(rows)-1], "ctrl+o")
	}
	got, scrolled := readLinkByScrolling(t, m, width, height)
	if got != url {
		t.Fatalf("rows seen while scrolling joined = %q\nwant the sign-in URL %q", got, url)
	}
	for range 200 {
		scrolled, _ = drive(t, scrolled, press(tea.KeyUp, 0, ""))
	}
	if again := linkRows(loginPanelText(t, scrolled)); !slices.Equal(again, top) {
		t.Fatalf("rows after scrolling back up = %q, want the first rows %q", again, top)
	}
}

func TestLoginURLScrollsWhenTheBodyCannotHoldIt(t *testing.T) {
	t.Parallel()
	url := loginURLWithHyphens(465)
	for _, size := range []struct{ width, height int }{{40, 24}, {40, 12}, {40, 8}, {40, 7}, {60, 12}, {100, 30}} {
		t.Run(fmt.Sprintf("%dx%d", size.width, size.height), func(t *testing.T) {
			t.Parallel()
			assertLoginURLScrolls(t, size.width, size.height, url)
		})
	}
}

func TestLoginURLStaysInReviewWhenNoBodyRowFits(t *testing.T) {
	t.Parallel()
	url := loginURLWithHyphens(465)
	m := waitingLoginPanel(t, 20, 6, &rpc.LoginAccountResponse{Type: "chatgpt", LoginID: "login-1", AuthURL: url})
	assertFrame(t, m.dockLines(), 20, 6)
	m, _ = drive(t, m, press('o', tea.ModCtrl, ""))
	if m.review == nil {
		t.Fatal("ctrl+o did not open the review")
	}
	var seen []string
	for range 400 {
		for _, line := range strings.Split(ansi.Strip(m.View().Content), "\n") {
			line = strings.TrimRight(line, " ")
			if !strings.HasPrefix(line, "Review ·") && !strings.HasPrefix(line, "↑↓ scroll") && !slices.Contains(seen, line) {
				seen = append(seen, line)
			}
		}
		m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	}
	if joined := strings.ReplaceAll(strings.Join(seen, ""), " ", ""); !strings.Contains(joined, url) {
		t.Fatalf("review rows joined = %q\nwant them to hold the sign-in URL %q", joined, url)
	}
}

func reviewRowsJoined(t *testing.T, m model, presses int) string {
	t.Helper()
	m, _ = drive(t, m, press('o', tea.ModCtrl, ""))
	if m.review == nil {
		t.Fatal("ctrl+o did not open the review")
	}
	var seen []string
	for range presses {
		for _, line := range strings.Split(ansi.Strip(m.View().Content), "\n") {
			line = strings.TrimRight(line, " ")
			if !strings.HasPrefix(line, "Review ·") && !strings.HasPrefix(line, "↑↓ scroll") && !slices.Contains(seen, line) {
				seen = append(seen, line)
			}
		}
		m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	}
	return strings.ReplaceAll(strings.Join(seen, ""), " ", "")
}

func TestLoginDeviceCodeStaysInReviewWhenNoBodyRowFits(t *testing.T) {
	t.Parallel()
	url := loginURLWithHyphens(200)
	response := &rpc.LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: "login-1", VerificationURL: url, UserCode: "WXYZ-9876"}
	assertPrintedRowsHideTheCode(t, response)
	m := waitingLoginPanel(t, 20, 6, response)
	assertFrame(t, m.dockLines(), 20, 6)
	for _, line := range m.dockLines() {
		if strings.HasPrefix(ansi.Strip(line), "│ ") {
			t.Fatalf("panel shows the body row %q at 20x6, want none", ansi.Strip(line))
		}
	}
	joined := reviewRowsJoined(t, m, 200)
	if !strings.Contains(joined, url) {
		t.Fatalf("review rows joined = %q\nwant them to hold the sign-in URL %q", joined, url)
	}
	if !strings.Contains(joined, "Usercode:WXYZ-9876") {
		t.Fatalf("review rows joined = %q\nwant them to hold the row User code: WXYZ-9876", joined)
	}
}

func TestLoginDeviceCodeIsWholeInTheFirstBodyRow(t *testing.T) {
	t.Parallel()
	const code = "WXYZ-9876"
	url := "https://auth.example.test/device"
	for _, width := range []int{60, 20} {
		t.Run(fmt.Sprint(width), func(t *testing.T) {
			t.Parallel()
			m := waitingLoginPanel(t, width, 12, &rpc.LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: "login-1", VerificationURL: url, UserCode: code})
			assertFrame(t, m.dockLines(), width, 12)
			rows := loginPanelText(t, m)
			if !strings.Contains(rows[0], code) {
				t.Fatalf("first panel row = %q, want the whole user code %q", rows[0], code)
			}
			if got := joinedPanelRows(t, rows[1:]); got != url {
				t.Fatalf("rows after the code joined = %q, want the whole link %q", got, url)
			}
		})
	}
}

func TestLoginDeviceCodeWiderThanTheBodyWrapsFromTheFirstBodyRow(t *testing.T) {
	t.Parallel()
	const code = "WXYZ-9876"
	url := "https://auth.example.test/device"
	m := waitingLoginPanel(t, 12, 16, &rpc.LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: "login-1", VerificationURL: url, UserCode: code})
	assertFrame(t, m.dockLines(), 12, 16)
	rows := loginPanelText(t, m)
	if !strings.HasPrefix(code, strings.TrimRight(rows[0], " ")) || strings.TrimSpace(rows[0]) == "" {
		t.Fatalf("first panel row = %q, want it to start the user code %q", rows[0], code)
	}
	if got := joinedPanelRows(t, rows); got != code+url {
		t.Fatalf("panel rows joined = %q, want the whole code then the whole link %q", got, code+url)
	}
}

func TestLoginDeviceCodeRowIsKeptOnlyWhenItFitsExactly(t *testing.T) {
	t.Parallel()
	const code = "WXYZ-9876"
	url := "https://auth.example.test/device"
	for _, tc := range []struct {
		width int
		first string
	}{{28, "Enter code WXYZ-9876 at"}, {27, code}} {
		t.Run(fmt.Sprint(tc.width), func(t *testing.T) {
			t.Parallel()
			m := waitingLoginPanel(t, tc.width, 12, &rpc.LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: "login-1", VerificationURL: url, UserCode: code})
			assertFrame(t, m.dockLines(), tc.width, 12)
			rows := loginPanelText(t, m)
			if got := strings.TrimRight(rows[0], " "); got != tc.first {
				t.Fatalf("first panel row at width %d = %q, want %q", tc.width, got, tc.first)
			}
		})
	}
}

func TestLoginDeviceCodeWithATabIsNeverCut(t *testing.T) {
	t.Parallel()
	const code = "A\tB-CDEF"
	url := "https://auth.example.test/device"
	for width := 13; width <= 60; width++ {
		t.Run(fmt.Sprint(width), func(t *testing.T) {
			t.Parallel()
			m := waitingLoginPanel(t, width, 40, &rpc.LoginAccountResponse{Type: "chatgptDeviceCode", LoginID: "login-1", VerificationURL: url, UserCode: code})
			assertFrame(t, m.dockLines(), width, 40)
			rows := loginPanelText(t, m)
			if first := strings.TrimSpace(rows[0]); !strings.HasPrefix(first, "A") && !strings.HasPrefix(first, "Enter code A") {
				t.Fatalf("first panel row at width %d = %q, want it to start the user code", width, rows[0])
			}
			joined := strings.ReplaceAll(joinedPanelRows(t, rows), " ", "")
			if want := "AB-CDEF"; !strings.Contains(joined, want) || !strings.HasSuffix(joined, url) {
				t.Fatalf("panel rows joined at width %d = %q, want the whole code %q and then the link", width, joined, want)
			}
		})
	}
}
