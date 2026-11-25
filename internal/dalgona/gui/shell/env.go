//go:build gui

package shell

import (
	"gioui.org/text"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type Env struct {
	Theme     theme.Name
	Shaper    *text.Shaper
	View      *state.View
	Submit    func(state.Intent)
	Window    state.WindowID
	Thread    state.ThreadKey
	Navigate  func(state.ThreadKey)
	Open      func(Route)
	Close     func()
	Announce  func(platform.Announcement)
	Clipboard func() platform.ClipboardReader
	Wake      func()
	Still     bool
	Modal     bool
	Deciding  bool
}

type Route struct {
	Panel   string
	Thread  state.ThreadKey
	Turn    string
	Context string
}
