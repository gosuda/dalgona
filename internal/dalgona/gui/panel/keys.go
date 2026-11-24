//go:build gui

package panel

import (
	"runtime"
	"slices"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	contextComposer   = "composer"
	contextTranscript = "transcript"
	contextRequest    = "request"
	contextList       = "list"
	contextTerminal   = "terminal"
	contextViewer     = "viewer"
	imeNote           = "While an input method composes text, it owns Enter, Escape, the arrows, Tab and its candidate keys."
	terminalNote      = "While the terminal has focus, keys go to the terminal program instead of the composer shortcuts."
)

type keyRow struct {
	keys   string
	action string
}

type keyGroup struct {
	title    string
	contexts []string
	rows     []keyRow
}

type modifiers struct {
	shortcut string
	alt      string
}

func platformModifiers(goos string) modifiers {
	if goos == "darwin" {
		return modifiers{shortcut: "Cmd", alt: "Option"}
	}
	return modifiers{shortcut: "Ctrl", alt: "Alt"}
}

func keyGroups(m modifiers) []keyGroup {
	return []keyGroup{
		{title: "Composer", contexts: []string{contextComposer}, rows: []keyRow{
			{"Enter", "Send, or steer while a turn runs"},
			{"Shift+Enter", "New line"},
			{m.alt + "+Enter", "Queue a follow-up while a turn runs"},
			{"ArrowUp, ArrowDown", "Recall sent messages from the first or last line"},
			{"Escape twice", "Interrupt the running turn within two seconds"},
			{"Escape", "Interrupt a rate-limit wait at once"},
			{"Ctrl+O", "Open the latest finished row"},
		}},
		{title: "Editing", contexts: []string{contextComposer}, rows: []keyRow{
			{m.shortcut + "+Z", "Undo"},
			{"Shift+" + m.shortcut + "+Z", "Redo"},
			{m.shortcut + "+X, " + m.shortcut + "+C, " + m.shortcut + "+V", "Cut, copy and paste"},
			{"Shift+" + m.shortcut + "+V", "Paste image"},
			{m.shortcut + "+A", "Select all"},
		}},
		{title: "Command or completion list", contexts: []string{contextComposer, contextList}, rows: []keyRow{
			{"ArrowUp, ArrowDown", "Select"},
			{"Enter, Tab", "Accept the selection"},
			{"Escape", "Close the list"},
		}},
		{title: "Requests", contexts: []string{contextRequest}, rows: []keyRow{
			{"y", "Accept the command or file change"},
			{"a", "Accept for this thread"},
			{"n", "Decline"},
			{"Escape", "Cancel the approval and interrupt the turn"},
			{"Escape", "Decline the requested workspace access"},
			{"Escape", "Dismiss a question with no answers"},
			{"Arrows, PageUp, PageDown, Home, End", "Scroll the request details"},
		}},
		{title: "Transcript", contexts: []string{contextTranscript}, rows: []keyRow{
			{"Ctrl+O", "Open the latest finished row"},
		}},
		{title: "Terminal", contexts: []string{contextTerminal}, rows: []keyRow{
			{"Ctrl+Tab, Ctrl+Shift+Tab", "Move focus out of the terminal"},
		}},
		{title: "Full output viewer", contexts: []string{contextViewer}, rows: []keyRow{
			{"Escape", "Close and return focus"},
		}},
		{title: "Dialogs", rows: []keyRow{
			{"Tab, Shift+Tab", "Move between the controls"},
			{"Escape", "Close and return focus"},
		}},
	}
}

func (g keyGroup) shown(context string) bool {
	return context == "" || len(g.contexts) == 0 || slices.Contains(g.contexts, context)
}

type keyPanel struct {
	context string
	goos    string
}

func (k *keyPanel) open(r shell.Route) {
	k.context = r.Context
	if k.goos == "" {
		k.goos = runtime.GOOS
	}
}

func (k *keyPanel) release(*[]state.Intent) {}

func (k *keyPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	var content []layout.Widget
	if k.context == contextTerminal {
		content = append(content, s.para(terminalNote))
	}
	for _, g := range keyGroups(platformModifiers(k.goos)) {
		if !g.shown(k.context) {
			continue
		}
		content = append(content, s.legend(g.title))
		for _, r := range g.rows {
			content = append(content, keyLine(s, r))
		}
	}
	content = append(content, s.note(imeNote))
	if k.context != contextTerminal {
		content = append(content, s.note(terminalNote))
	}
	n, row := rows(content)
	return s.show(gtx, Content{Title: titles["help"], Rows: n, Row: row})
}

func keyLine(s *scene, r keyRow) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{Alignment: layout.Baseline}.Layout(gtx,
			layout.Flexed(1, s.code(r.keys)),
			layout.Flexed(2, s.para(r.action)),
		)
	}
}
