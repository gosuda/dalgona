package vtstate

import "github.com/charmbracelet/x/ansi"

type mode struct {
	number uint16
	dec    bool
}

var supportedModes = [...]mode{
	{number: 20},
	{number: 1, dec: true},
	{number: 6, dec: true},
	{number: 7, dec: true},
	{number: 9, dec: true},
	{number: 25, dec: true},
	{number: 66, dec: true},
	{number: 69, dec: true},
	{number: 1000, dec: true},
	{number: 1001, dec: true},
	{number: 1002, dec: true},
	{number: 1003, dec: true},
	{number: 1004, dec: true},
	{number: 1006, dec: true},
	{number: 1047, dec: true},
	{number: 1048, dec: true},
	{number: 1049, dec: true},
	{number: 2004, dec: true},
	{number: 2048, dec: true},
}

type modeSet uint32

var (
	modeLNM            = modeBit(mode{number: 20})
	modeCursorKeys     = modeBit(mode{number: 1, dec: true})
	modeOrigin         = modeBit(mode{number: 6, dec: true})
	modeAutoWrap       = modeBit(mode{number: 7, dec: true})
	modeMouseX10       = modeBit(mode{number: 9, dec: true})
	modeCursorVisible  = modeBit(mode{number: 25, dec: true})
	modeKeypad         = modeBit(mode{number: 66, dec: true})
	modeLeftRight      = modeBit(mode{number: 69, dec: true})
	modeMouseNormal    = modeBit(mode{number: 1000, dec: true})
	modeMouseButton    = modeBit(mode{number: 1002, dec: true})
	modeMouseAny       = modeBit(mode{number: 1003, dec: true})
	modeFocus          = modeBit(mode{number: 1004, dec: true})
	modeMouseSGR       = modeBit(mode{number: 1006, dec: true})
	modeAltScreen      = modeBit(mode{number: 1047, dec: true})
	modeSaveCursor     = modeBit(mode{number: 1048, dec: true})
	modeAltSaveCursor  = modeBit(mode{number: 1049, dec: true})
	modeBracketedPaste = modeBit(mode{number: 2004, dec: true})
	modeInBandResize   = modeBit(mode{number: 2048, dec: true})
	defaultModes       = modeAutoWrap | modeCursorVisible
)

func modeBit(m mode) modeSet {
	for i, s := range supportedModes {
		if s == m {
			return 1 << i
		}
	}
	return 0
}

func (s modeSet) has(m modeSet) bool {
	return s&m != 0
}

type InputModes struct {
	ApplicationCursor bool
	ApplicationKeypad bool
	LineFeedNewLine   bool
	BracketedPaste    bool
	FocusEvents       bool
	MouseX10          bool
	MouseNormal       bool
	MouseButton       bool
	MouseAny          bool
	MouseSGR          bool
}

func (t *Terminal) InputModes() InputModes {
	return InputModes{
		ApplicationCursor: t.modes.has(modeCursorKeys),
		ApplicationKeypad: t.modes.has(modeKeypad),
		LineFeedNewLine:   t.modes.has(modeLNM),
		BracketedPaste:    t.modes.has(modeBracketedPaste),
		FocusEvents:       t.modes.has(modeFocus),
		MouseX10:          t.modes.has(modeMouseX10),
		MouseNormal:       t.modes.has(modeMouseNormal),
		MouseButton:       t.modes.has(modeMouseButton),
		MouseAny:          t.modes.has(modeMouseAny),
		MouseSGR:          t.modes.has(modeMouseSGR),
	}
}

func (t *Terminal) handleMode(params ansi.Params, set, dec bool) {
	for _, p := range params {
		number := p.Param(-1)
		if number < 0 || number > 0xffff {
			continue
		}
		bit := modeBit(mode{number: uint16(number), dec: dec})
		if bit != 0 {
			t.setMode(bit, set)
		}
	}
}

func (t *Terminal) setMode(bit modeSet, set bool) {
	if set {
		t.modes |= bit
	} else {
		t.modes &^= bit
	}
	switch bit {
	case modeAltScreen:
		t.switchScreen(set)
	case modeSaveCursor:
		t.saveOrRestoreCursor(set)
	case modeAltSaveCursor:
		t.enterOrLeaveAlternate(set)
	case modeInBandResize:
		if set {
			t.replyResize()
		}
	}
}

func (t *Terminal) saveOrRestoreCursor(save bool) {
	if save {
		t.saveCursor()
		return
	}
	t.restoreCursor()
}

func (t *Terminal) enterOrLeaveAlternate(enter bool) {
	if enter {
		t.saveCursor()
		t.switchScreen(true)
		return
	}
	t.switchScreen(false)
	t.restoreCursor()
}

func (t *Terminal) switchScreen(alternate bool) {
	target := 0
	if alternate {
		target = 1
	}
	if t.active == target {
		return
	}
	t.active = target
	if alternate {
		t.screen().clear()
		t.screen().setCursor(0, 0, false)
	}
}

func (t *Terminal) requestMode(params ansi.Params, dec bool) {
	n, _, ok := params.Param(0, 0)
	if !ok || n <= 0 || n > 0xffff {
		return
	}
	bit := modeBit(mode{number: uint16(n), dec: dec})
	setting := ansi.ModeNotRecognized
	switch {
	case bit == 0:
	case t.modes.has(bit):
		setting = ansi.ModeSet
	default:
		setting = ansi.ModeReset
	}
	var m ansi.Mode = ansi.ANSIMode(n)
	if dec {
		m = ansi.DECMode(n)
	}
	t.reply(ansi.ReportMode(m, setting))
}
