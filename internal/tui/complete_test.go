package tui

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func paletteModel(t *testing.T) model {
	t.Helper()
	m := newTestModel(nil, Options{}, nil)
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
	assertContains(t, dockText(m), "┃ /resume")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	assertContains(t, dockText(m), "┃ /model")
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
	m := newTestModel(caller, Options{Cwd: "/work"}, nil)
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
	m := newTestModel(&fileSearchCaller{}, Options{Cwd: "/work"}, nil)
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
	m := newTestModel(caller, Options{Cwd: "/work"}, nil)
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
	m := newTestModel(caller, Options{Cwd: "/work"}, nil)
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
	caller := &fileSearchCaller{hits: []fileHit{{Path: "pkg/model.go"}}}
	t.Run("deleted character", func(t *testing.T) {
		m := newTestModel(caller, Options{Cwd: "/work"}, nil)
		m, command := typeCommand(t, m, "@mod")
		m = runFileSearch(t, m, command)
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyTab})
		if got := m.input.composer.Value(); got != "@mo" {
			t.Fatalf("tab before the reply gave %q, want %q", got, "@mo")
		}
	})
	t.Run("typed character", func(t *testing.T) {
		m := newTestModel(caller, Options{Cwd: "/work"}, nil)
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
	m := newTestModel(caller, Options{Cwd: "/work"}, nil)
	m.thread.id = "t1"
	m, _ = typeCommand(t, m, "@mod")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if got := m.input.composer.Value(); got != "@mod" {
		t.Fatalf("enter with no file rows gave %q, want the draft %q kept", got, "@mod")
	}
}
