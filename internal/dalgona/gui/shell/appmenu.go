//go:build gui

package shell

import (
	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	appMenuLabel      = "Menu"
	settingsRoute     = "settings"
	diagnosticsRoute  = "doctor"
	contextComposer   = "composer"
	contextTranscript = "transcript"
	contextRequest    = "request"
)

const (
	appNewThread = iota
	appNewWindow
	appKeys
	appSettings
	appDiagnostics
	appCloseWindow
)

var appEntries = [...]string{
	appNewThread:   "New thread",
	appNewWindow:   "New window",
	appKeys:        "Keyboard shortcuts",
	appSettings:    "Settings",
	appDiagnostics: "Diagnostics",
	appCloseWindow: "Close window",
}

type appMenu struct {
	toggle widget.Clickable
	items  [len(appEntries)]widget.Clickable
	open   bool
}

func (s *Shell) updateAppMenu(gtx layout.Context) {
	if s.apps.toggle.Clicked(gtx) {
		s.apps.open = !s.apps.open
		s.menu.open = false
		if s.apps.open {
			s.focus.request(&s.apps.items[0])
		}
	}
	for i := range s.apps.items {
		if s.apps.items[i].Clicked(gtx) {
			s.apps.open = false
			s.runApp(i)
		}
	}
}

func (s *Shell) runApp(entry int) {
	opener := &s.apps.toggle
	switch entry {
	case appNewThread:
		s.openPanel(Route{Panel: newRoute, Thread: state.ThreadKey{Endpoint: s.open.Endpoint}}, opener)
	case appNewWindow:
		run(s.newWindow)
	case appKeys:
		s.openPanel(Route{Panel: helpRoute, Thread: s.open, Context: s.appContext()}, opener)
	case appSettings:
		s.openPanel(Route{Panel: settingsRoute, Thread: s.open}, opener)
	case appDiagnostics:
		s.openPanel(Route{Panel: diagnosticsRoute, Thread: s.open}, opener)
	case appCloseWindow:
		run(s.closeWindow)
	}
}

func run(action func()) {
	if action != nil {
		action()
	}
}

func (s *Shell) trackContext(gtx layout.Context) {
	switch {
	case s.composerFocused(gtx):
		s.context = contextComposer
	case s.parts.Transcript != nil && gtx.Focused(s.parts.Transcript.FocusTag()):
		s.context = contextTranscript
	}
}

func (s *Shell) appContext() string {
	switch {
	case s.requestShowing():
		return contextRequest
	case s.context != "":
		return s.context
	case s.open.Thread != "":
		return contextTranscript
	}
	return ""
}

func (s *Shell) dismissMenus() {
	if s.menu.open {
		s.menu.open = false
		s.focus.request(&s.menu.toggle)
	}
	if s.apps.open {
		s.apps.open = false
		s.focus.request(&s.apps.toggle)
	}
}

func (s *Shell) appMenuButton(gtx layout.Context, f frame) layout.Dimensions {
	toggle := button{click: &s.apps.toggle, label: appMenuLabel, id: "window-commands", expandable: true, expanded: s.apps.open}
	if !s.apps.open {
		return s.dropdown(gtx, f, toggle, nil)
	}
	items := make([]button, len(appEntries))
	for i, label := range appEntries {
		items[i] = button{click: &s.apps.items[i], label: label, role: platform.RoleMenuItem}
	}
	return s.dropdown(gtx, f, toggle, items)
}
