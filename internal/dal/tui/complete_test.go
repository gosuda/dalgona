package tui

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func paletteModel(t *testing.T) model {
	t.Helper()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	return m
}

func typeKeys(t *testing.T, m model, text string) model {
	t.Helper()
	for _, r := range text {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	return m
}

func TestSlashPaletteFiltersAndCompletes(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/mo")
	assertContains(t, dockText(m), "/model")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
	if got := m.input.composer.Value(); got != "/model " {
		t.Fatalf("tab gave %q, want %q", got, "/model ")
	}
}

func TestPopupStaysOpenOnFirstToken(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "mo")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	m = typeKeys(t, m, "/")
	assertContains(t, dockText(m), "/model")
	if _, row, col := m.dock(); row != 0 || col != promptCells+1 {
		t.Fatalf("caret at row %d column %d, want row 0 column %d", row, col, promptCells+1)
	}
}

func TestPopupSteady(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/")
	if got := len(m.dockLines()); got != 11 {
		t.Fatalf("dock with the palette open has %d lines, want 11", got)
	}
	keys := make([]tea.KeyPressMsg, 0, 100)
	for range 50 {
		keys = append(keys, press('a', 0, "a"))
	}
	for range 50 {
		keys = append(keys, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	assertSteadyDock(t, m, keys)
}

func TestPaletteHighlightMovesWithArrows(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	assertContains(t, dockText(m), "┃ ● /resume")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	assertContains(t, dockText(m), "┃ ● /model")
}

func TestPaletteEscKeepsDraftAndTypingReopens(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/m")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after esc has %d lines, want 3", got)
	}
	if got := m.input.composer.Value(); got != "/m" {
		t.Fatalf("esc changed the draft to %q", got)
	}
	m = typeKeys(t, m, "o")
	assertContains(t, dockText(m), "/model")
}

func TestPaletteClosesAfterFirstToken(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/mo ")
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after a space has %d lines, want 3", got)
	}
}

func TestPaletteIgnoresDoubleSlash(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "//")
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock for // has %d lines, want 3", got)
	}
}

func TestPaletteEnterCompletesCommandWithArgument(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "/mo")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "/model " {
		t.Fatalf("enter gave %q, want %q", got, "/model ")
	}
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after completing has %d lines, want 3", got)
	}
}

func TestPaletteCompletionKeepsTextAfterToken(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), " arg")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	m = typeKeys(t, m, "/mo")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
	if got := m.input.composer.Value(); got != "/model arg" {
		t.Fatalf("tab gave %q, want %q", got, "/model arg")
	}
}

func TestPaletteCompletionKeepsChips(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(12)
	for _, key := range []tea.KeyPressMsg{{Code: tea.KeyTab}, {Code: tea.KeyEnter}} {
		m := typeKeys(t, paletteModel(t), " ")
		m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
		m = typeKeys(t, m, "/mo")
		m, _ = drive(t, m, key)
		if got, want := m.input.composer.Expanded(), "/model "+pasted; got != want {
			t.Fatalf("%s gave %q, want %q", key.String(), got, want)
		}
		if hasPrivateUse(m.input.composer.Expanded()) {
			t.Fatalf("%s left a chip atom in the sent text: %q", key.String(), m.input.composer.Expanded())
		}
		assertContains(t, dockText(m), "[Paste #1 +12 lines]")
		if got := len(m.input.history.entries); got != 0 {
			t.Fatalf("%s sent a prompt: history holds %d entries", key.String(), got)
		}
	}
}

func chipAfterToken(t *testing.T, pasted, after string) model {
	t.Helper()
	m := paletteModel(t)
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	m = typeKeys(t, m, after)
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	return typeKeys(t, m, "/new")
}

func requireCompletedBesideChip(t *testing.T, key tea.KeyPressMsg, pasted, after string) {
	t.Helper()
	m := chipAfterToken(t, pasted, after)
	if m.input.popup == nil || len(m.input.popup.items) == 0 || m.input.popup.selected() != "new" {
		t.Fatalf("popup before %s does not list /new: %+v", key.String(), m.input.popup)
	}
	m, _ = drive(t, m, key)
	if got, want := m.input.composer.Expanded(), "/new "+pasted+after; got != want {
		t.Fatalf("%s gave %q, want %q", key.String(), got, want)
	}
	if got := len(m.input.composer.chips); got != 1 {
		t.Fatalf("%s left %d chips, want 1", key.String(), got)
	}
	if got, want := m.input.composer.cursor, len("/new "); got != want {
		t.Fatalf("%s left the caret at %d, want %d", key.String(), got, want)
	}
	if m.input.popup != nil {
		t.Fatalf("%s left the palette open", key.String())
	}
	if got := len(m.input.history.entries); got != 0 {
		t.Fatalf("%s sent a prompt: history holds %d entries", key.String(), got)
	}
}

func TestPaletteKeepsChipTouchingTheToken(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(12)
	for _, key := range []tea.KeyPressMsg{{Code: tea.KeyTab}, {Code: tea.KeyEnter}} {
		for _, after := range []string{"", " tail"} {
			t.Run(key.String()+after, func(t *testing.T) {
				t.Parallel()
				requireCompletedBesideChip(t, key, pasted, after)
			})
		}
	}
}

func TestPaletteTokenEndsAtChip(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(12)
	m := typeKeys(t, paletteModel(t), "/new")
	if m.input.popup == nil {
		t.Fatal("typing /new did not open the palette")
	}
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	if m.input.popup != nil {
		t.Fatalf("the palette stays open with the caret past the chip: %+v", m.input.popup)
	}
	if got, want := m.input.composer.Expanded(), "/new"+pasted; got != want {
		t.Fatalf("paste gave %q, want %q", got, want)
	}
}

func TestPaletteEnterKeepsTextAfterTokenAndRunsNothing(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), " arg")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
	m = typeKeys(t, m, "/new")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "/new arg" {
		t.Fatalf("enter gave %q, want %q", got, "/new arg")
	}
	if got := len(m.input.history.entries); got != 0 {
		t.Fatalf("enter sent a prompt: history holds %d entries", got)
	}
}

func TestPaletteClosedByReview(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, reviewModel(t, 80, 3), "/")
	m = rawUpdate(t, m, press('o', tea.ModCtrl, ""))
	if m.review == nil {
		t.Fatal("ctrl+o did not open the review")
	}
	m = rawUpdate(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if m.review != nil {
		t.Fatal("esc did not close the review")
	}
	if got := len(m.dockLines()); got != 3 {
		t.Fatalf("dock after the review closes has %d lines, want 3", got)
	}
}

func TestPaletteWithoutMatchLeavesArrowsToComposer(t *testing.T) {
	t.Parallel()
	m := paletteModel(t)
	m.input.history.add("earlier prompt", "")
	m = typeKeys(t, m, "/zzz")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if got := m.input.composer.Value(); got != "earlier prompt" {
		t.Fatalf("up with no matching command gave %q, want the recalled prompt", got)
	}
}

type fileSearchCaller struct {
	fakeCaller
	hits []fileHit
	err  error
}

func (f *fileSearchCaller) Call(ctx context.Context, method string, params, result any) error {
	if method != "fuzzyFileSearch" {
		return f.fakeCaller.Call(ctx, method, params, result)
	}
	if f.err != nil {
		return f.err
	}
	if reply, ok := result.(*fileSearchReply); ok {
		reply.Files = f.hits
	}
	return nil
}

func typeCommand(t *testing.T, m model, text string) (model, tea.Cmd) {
	t.Helper()
	var command tea.Cmd
	for _, r := range text {
		m, command = drive(t, m, press(r, 0, string(r)))
	}
	return m, command
}

func runFileSearch(t *testing.T, m model, command tea.Cmd) model {
	t.Helper()
	if command == nil {
		return m
	}
	switch msg := command().(type) {
	case tea.BatchMsg:
		for _, next := range msg {
			m = runFileSearch(t, m, next)
		}
	case debounceMsg, searchResultMsg:
		var next tea.Cmd
		m, next = drive(t, m, msg)
		m = runFileSearch(t, m, next)
	}
	return m
}

func TestAtFileCompletionInsertsPath(t *testing.T) {
	t.Parallel()
	caller := &fileSearchCaller{hits: []fileHit{{Path: "pkg/model.go"}}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m, command := typeCommand(t, m, "@modl")
	m = runFileSearch(t, m, command)
	assertContains(t, dockText(m), "pkg/model.go")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "@pkg/model.go " {
		t.Fatalf("enter gave %q, want %q", got, "@pkg/model.go ")
	}
}

func TestFileSearchStaleResultIgnored(t *testing.T) {
	t.Parallel()
	m := newTestModel(&fileSearchCaller{}, Options{Cwd: "/work"})
	m, _ = typeCommand(t, m, "@a")
	first := m.input.popup.seq
	m, _ = typeCommand(t, m, "b")
	second := m.input.popup.seq
	m, _ = drive(t, m, searchResultMsg{seq: second, files: []fileHit{{Path: "fresh.go"}}})
	m, _ = drive(t, m, searchResultMsg{seq: first, files: []fileHit{{Path: "stale.go"}}})
	view := dockText(m)
	assertContains(t, view, "fresh.go")
	if strings.Contains(view, "stale.go") {
		t.Fatalf("stale result shown:\n%s", view)
	}
}

func TestFilePopupSteady(t *testing.T) {
	t.Parallel()
	m := typeKeys(t, paletteModel(t), "@")
	if got := len(m.dockLines()); got != 11 {
		t.Fatalf("dock with the file palette open has %d lines, want 11", got)
	}
	keys := make([]tea.KeyPressMsg, 0, 100)
	for range 50 {
		keys = append(keys, press('a', 0, "a"))
	}
	for range 50 {
		keys = append(keys, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	assertSteadyDock(t, m, keys)
}

func TestFileSearchUnavailableShowsOnce(t *testing.T) {
	t.Parallel()
	caller := &fileSearchCaller{err: &rpc.RPCError{Code: -32601, Message: "method not found"}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m, command := typeCommand(t, m, "@x")
	m = runFileSearch(t, m, command)
	assertContains(t, dockText(m), "File search is unavailable on this server")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	m, command = typeCommand(t, m, "@y")
	m = runFileSearch(t, m, command)
	if strings.Contains(dockText(m), "unavailable") {
		t.Fatalf("later error showed the unavailable row:\n%s", dockText(m))
	}
}

func TestFilePaletteDropsRowsWhenQueryChanges(t *testing.T) {
	t.Parallel()
	caller := &fileSearchCaller{hits: []fileHit{{Path: "pkg/model.go"}}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m, command := typeCommand(t, m, "@mod")
	m = runFileSearch(t, m, command)
	assertContains(t, dockText(m), "pkg/model.go")
	for range 3 {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	}
	if view := dockText(m); strings.Contains(view, "pkg/model.go") {
		t.Fatalf("file palette kept rows of the query that was deleted:\n%s", view)
	}
}

func TestFilePaletteTabIgnoresOlderHits(t *testing.T) {
	t.Parallel()
	t.Run("deleted character", func(t *testing.T) {
		t.Parallel()
		caller := &fileSearchCaller{hits: []fileHit{{Path: "pkg/model.go"}}}
		m := newTestModel(caller, Options{Cwd: "/work"})
		m, command := typeCommand(t, m, "@mod")
		m = runFileSearch(t, m, command)
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
		if got := m.input.composer.Value(); got != "@mo" {
			t.Fatalf("tab before the reply gave %q, want %q", got, "@mo")
		}
	})
	t.Run("typed character", func(t *testing.T) {
		t.Parallel()
		caller := &fileSearchCaller{hits: []fileHit{{Path: "pkg/model.go"}}}
		m := newTestModel(caller, Options{Cwd: "/work"})
		m, command := typeCommand(t, m, "@mod")
		m = runFileSearch(t, m, command)
		m, _ = typeCommand(t, m, "e")
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
		if got := m.input.composer.Value(); got != "@mode" {
			t.Fatalf("tab before the reply gave %q, want %q", got, "@mode")
		}
	})
}

func TestFilePaletteEnterWithoutRowsKeepsDraft(t *testing.T) {
	t.Parallel()
	caller := &fileSearchCaller{hits: []fileHit{{Path: "pkg/model.go"}}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m.thread.id = "t1"
	m, _ = typeCommand(t, m, "@mod")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "@mod" {
		t.Fatalf("enter with no file rows gave %q, want the draft %q kept", got, "@mod")
	}
}

func submitBesideChip(t *testing.T, caller *fakeCaller, typed string) model {
	t.Helper()
	m := newTestModel(caller, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	m = typeKeys(t, m, typed)
	m, _ = drive(t, m, tea.PasteMsg{Content: pasteLines(12)})
	if len(m.input.composer.chips) != 1 || m.input.popup != nil {
		t.Fatalf("draft has %d chips and popup %+v, want one chip and a closed palette", len(m.input.composer.chips), m.input.popup)
	}
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command != nil {
		m = runCommand(t, m, command, "")
	}
	return m
}

func TestSubmittedCommandEndsAtChipLikeThePalette(t *testing.T) {
	t.Parallel()
	tests := []struct {
		command string
		opened  func(model) bool
	}{
		{command: "/new", opened: func(m model) bool { return m.confirm != nil && m.confirm.kind == confirmNew }},
		{command: "/help", opened: func(m model) bool { return m.help != nil }},
	}
	for _, test := range tests {
		for _, between := range []string{"", " "} {
			t.Run(test.command+between, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := submitBesideChip(t, caller, test.command+between)
				if !test.opened(m) {
					t.Fatalf("enter on %q and a chip ran no command; notice %q", test.command+between, m.notice)
				}
				if len(caller.calls) != 0 || strings.Contains(m.notice, "unknown command") {
					t.Fatalf("enter on %q and a chip made calls %#v and notice %q, want the command alone", test.command+between, caller.calls, m.notice)
				}
			})
		}
	}
}

func completeFileBesideChip(t *testing.T, side string, key tea.KeyPressMsg) (model, string) {
	t.Helper()
	pasted := pasteLines(12)
	caller := &fileSearchCaller{hits: []fileHit{{Path: "src/main.go"}}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	want := pasted + "@src/main.go "
	if side == "after" {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyHome})
		want = "@src/main.go " + pasted
	}
	m, command := typeCommand(t, m, "@sr")
	m = runFileSearch(t, m, command)
	if popup := m.input.popup; popup == nil || popup.kind != popupFile || popup.query != "sr" || len(popup.items) != 1 {
		t.Fatalf("popup = %+v, want one file for the query sr without the chip", popup)
	}
	m, _ = drive(t, m, key)
	return m, want
}

func TestFilePaletteEndsTheTokenAtAChip(t *testing.T) {
	t.Parallel()
	for _, side := range []string{"before", "after"} {
		for _, key := range []tea.KeyPressMsg{{Code: tea.KeyTab}, {Code: tea.KeyEnter}} {
			t.Run(side+"/"+key.String(), func(t *testing.T) {
				t.Parallel()
				m, want := completeFileBesideChip(t, side, key)
				if got := m.input.composer.Expanded(); got != want {
					t.Fatalf("%s gave %q, want %q", key.String(), got, want)
				}
				if got := len(m.input.composer.chips); got != 1 {
					t.Fatalf("%s left %d chips, want 1", key.String(), got)
				}
			})
		}
	}
}

func TestSubmittedPromptTouchingAChipKeepsItsText(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(12)
	tests := []struct{ typed, want string }{
		{typed: "//ab", want: "/ab" + pasted},
		{typed: "/a/b", want: "/a/b" + pasted},
	}
	for _, test := range tests {
		t.Run(test.typed, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := newTestModel(caller, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
			m.thread.id = "t1"
			m = typeKeys(t, m, test.typed)
			m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			runCommand(t, m, command, "enter did not send the prompt; notice: "+m.notice)
			assertTurnStartText(t, caller, test.want)
		})
	}
}
