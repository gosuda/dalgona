//go:build gui

package shell

import (
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestApplicationMenuOpensTheKeyListForTheComposerItLeft(t *testing.T) {
	h := newHarness(t)
	key0 := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := connectedView(manyRows(1)...)
	snap.Threads = []*state.ThreadView{openThread(key0, "Thread 0")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.focus(v, h.composer.FocusTag())
	h.click(v, appMenuLabel)
	h.click(v, appEntries[appKeys])
	got := h.panels["help"].opened
	if len(got) != 1 || got[0].Context != contextComposer || got[0].Thread != key0 {
		t.Fatalf("the menu opened help with %+v, want one route for the composer of %v", got, key0)
	}
}

func TestApplicationMenuOpensTheKeyListForTheTranscriptItLeft(t *testing.T) {
	h := newHarness(t)
	key0 := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := connectedView(manyRows(1)...)
	snap.Threads = []*state.ThreadView{openThread(key0, "Thread 0")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.focus(v, h.composer.FocusTag())
	h.focus(v, h.transcript.FocusTag())
	h.click(v, appMenuLabel)
	h.click(v, appEntries[appKeys])
	if got := h.panels["help"].opened; len(got) != 1 || got[0].Context != contextTranscript {
		t.Fatalf("the menu opened help with %+v, want the transcript context", got)
	}
}

func TestApplicationMenuRunsTheWindowActionsOnce(t *testing.T) {
	h := newHarness(t)
	v := connectedView().View()
	h.settle(v)
	h.click(v, appMenuLabel)
	h.click(v, appEntries[appNewWindow])
	h.click(v, appMenuLabel)
	h.click(v, appEntries[appCloseWindow])
	if h.windows.opened != 1 || h.windows.closed != 1 {
		t.Fatalf("window actions ran %+v, want one of each", h.windows)
	}
}

func TestApplicationMenuRoutesSettingsAndDiagnostics(t *testing.T) {
	h := newHarness(t)
	v := connectedView().View()
	h.settle(v)
	h.click(v, appMenuLabel)
	h.click(v, appEntries[appSettings])
	h.shell.closePanel()
	h.frame(v)
	h.click(v, appMenuLabel)
	h.click(v, appEntries[appDiagnostics])
	if len(h.panels[settingsRoute].opened) != 1 || len(h.panels[diagnosticsRoute].opened) != 1 {
		t.Fatalf("settings opened %d times and diagnostics %d times, want once each", len(h.panels[settingsRoute].opened), len(h.panels[diagnosticsRoute].opened))
	}
}

func TestEscapeClosesTheApplicationMenuAndKeepsItsButtonFocused(t *testing.T) {
	h := newHarness(t)
	v := connectedView().View()
	h.settle(v)
	h.click(v, appMenuLabel)
	h.frame(v)
	if _, ok := h.button(appEntries[appSettings]); !ok {
		t.Fatalf("the menu did not open: %v", h.labels())
	}
	h.press(v, key.NameEscape, 0)
	h.frame(v)
	if _, ok := h.button(appEntries[appSettings]); ok {
		t.Fatal("Escape left the application menu open")
	}
	if !h.shell.focus.usable(&h.shell.apps.toggle) || h.shell.focus.focusedLast != &h.shell.apps.toggle {
		t.Fatal("focus did not return to the menu button")
	}
}
