package terminal

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

func TestKeysEncodeAsXtermPCStyleSequences(t *testing.T) {
	normal := vtstate.InputModes{}
	application := vtstate.InputModes{ApplicationCursor: true}
	lineFeed := vtstate.InputModes{LineFeedNewLine: true}
	cases := []struct {
		key   Key
		mods  Modifiers
		modes vtstate.InputModes
		want  string
	}{
		{KeyUp, 0, normal, "\x1b[A"},
		{KeyDown, 0, normal, "\x1b[B"},
		{KeyRight, 0, normal, "\x1b[C"},
		{KeyLeft, 0, normal, "\x1b[D"},
		{KeyUp, 0, application, "\x1bOA"},
		{KeyLeft, 0, application, "\x1bOD"},
		{KeyHome, 0, normal, "\x1b[H"},
		{KeyEnd, 0, normal, "\x1b[F"},
		{KeyHome, 0, application, "\x1bOH"},
		{KeyEnd, 0, application, "\x1bOF"},
		{KeyUp, ModShift, normal, "\x1b[1;2A"},
		{KeyUp, ModAlt, application, "\x1b[1;3A"},
		{KeyRight, ModCtrl, normal, "\x1b[1;5C"},
		{KeyLeft, ModCtrl | ModShift, normal, "\x1b[1;6D"},
		{KeyEnd, ModCtrl | ModAlt | ModShift, normal, "\x1b[1;8F"},
		{KeyInsert, 0, normal, "\x1b[2~"},
		{KeyDelete, 0, normal, "\x1b[3~"},
		{KeyPageUp, 0, normal, "\x1b[5~"},
		{KeyPageDown, 0, application, "\x1b[6~"},
		{KeyPageUp, ModShift, normal, "\x1b[5;2~"},
		{KeyDelete, ModCtrl, normal, "\x1b[3;5~"},
		{KeyF1, 0, normal, "\x1bOP"},
		{KeyF2, 0, normal, "\x1bOQ"},
		{KeyF3, 0, normal, "\x1bOR"},
		{KeyF4, 0, normal, "\x1bOS"},
		{KeyF1, ModCtrl, normal, "\x1b[1;5P"},
		{KeyF5, 0, normal, "\x1b[15~"},
		{KeyF6, 0, normal, "\x1b[17~"},
		{KeyF7, 0, normal, "\x1b[18~"},
		{KeyF8, 0, normal, "\x1b[19~"},
		{KeyF9, 0, normal, "\x1b[20~"},
		{KeyF10, 0, normal, "\x1b[21~"},
		{KeyF11, 0, normal, "\x1b[23~"},
		{KeyF12, 0, normal, "\x1b[24~"},
		{KeyF5, ModShift, normal, "\x1b[15;2~"},
		{KeyEnter, 0, normal, "\r"},
		{KeyEnter, 0, lineFeed, "\r\n"},
		{KeyEnter, ModAlt, normal, "\x1b\r"},
		{KeyTab, 0, normal, "\t"},
		{KeyTab, ModShift, normal, "\x1b[Z"},
		{KeyBackspace, 0, normal, "\x7f"},
		{KeyBackspace, ModCtrl, normal, "\b"},
		{KeyBackspace, ModAlt, normal, "\x1b\x7f"},
		{KeyEscape, 0, normal, "\x1b"},
	}
	for _, c := range cases {
		if got := string(EncodeKey(nil, c.key, c.mods, c.modes)); got != c.want {
			t.Fatalf("key %d mods %b modes %+v = %q, want %q", c.key, c.mods, c.modes, got, c.want)
		}
	}
}

func TestTextEncodesControlAndMetaPrefixes(t *testing.T) {
	cases := []struct {
		text string
		mods Modifiers
		want string
	}{
		{"a", 0, "a"},
		{"한", 0, "한"},
		{"c", ModCtrl, "\x03"},
		{"C", ModCtrl, "\x03"},
		{"[", ModCtrl, "\x1b"},
		{" ", ModCtrl, "\x00"},
		{"?", ModCtrl, "\x7f"},
		{"x", ModAlt, "\x1bx"},
		{"d", ModCtrl | ModAlt, "\x1b\x04"},
	}
	for _, c := range cases {
		if got := string(EncodeText(nil, c.text, c.mods)); got != c.want {
			t.Fatalf("text %q mods %b = %q, want %q", c.text, c.mods, got, c.want)
		}
	}
}

func TestTerminalModesDriveEncoding(t *testing.T) {
	term := vtstate.New(20, 5)
	term.Feed([]byte("\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1000h\x1b[?1006h"))
	modes := term.InputModes()
	if got := string(EncodeKey(nil, KeyUp, 0, modes)); got != "\x1bOA" {
		t.Fatalf("up after DECCKM = %q", got)
	}
	if got := string(EncodePaste(nil, "a\r\nb\nc", modes)); got != "\x1b[200~a\rb\rc\x1b[201~" {
		t.Fatalf("bracketed paste = %q", got)
	}
	if got := string(EncodeFocus(nil, false, modes)); got != "\x1b[O" {
		t.Fatalf("focus out = %q", got)
	}
	if got := string(EncodeMouse(nil, MouseEvent{Column: 4, Row: 1, Button: MouseLeft}, modes)); got != "\x1b[<0;5;2M" {
		t.Fatalf("SGR press = %q", got)
	}
	term.Feed([]byte("\x1b[?1l\x1b[?2004l\x1b[?1004l\x1b[?1000l"))
	modes = term.InputModes()
	if got := string(EncodeKey(nil, KeyUp, 0, modes)); got != "\x1b[A" {
		t.Fatalf("up after DECCKM reset = %q", got)
	}
	if got := string(EncodePaste(nil, "x\x1b[201~y", modes)); got != "x\x1b[201~y" {
		t.Fatalf("plain paste = %q", got)
	}
	if got := EncodeFocus(nil, true, modes); len(got) != 0 {
		t.Fatalf("focus without mode 1004 = %q", got)
	}
	if got := EncodeMouse(nil, MouseEvent{Button: MouseLeft}, modes); len(got) != 0 {
		t.Fatalf("mouse without tracking = %q", got)
	}
}

func TestBracketedPasteCannotEndItsOwnBracket(t *testing.T) {
	modes := vtstate.InputModes{BracketedPaste: true}
	if got := string(EncodePaste(nil, "x\x1b[201~rm -rf ~\n", modes)); got != "\x1b[200~xrm -rf ~\r\x1b[201~" {
		t.Fatalf("paste = %q", got)
	}
}

func TestMouseReportsFollowTrackingModes(t *testing.T) {
	x10 := vtstate.InputModes{MouseX10: true}
	normal := vtstate.InputModes{MouseNormal: true}
	button := vtstate.InputModes{MouseButton: true, MouseSGR: true}
	anyMotion := vtstate.InputModes{MouseAny: true, MouseSGR: true}
	sgr := vtstate.InputModes{MouseNormal: true, MouseSGR: true}
	cases := []struct {
		name  string
		ev    MouseEvent
		modes vtstate.InputModes
		want  string
	}{
		{"x10 press", MouseEvent{Column: 2, Row: 3, Button: MouseLeft}, x10, "\x1b[M #$"},
		{"x10 ignores modifiers", MouseEvent{Column: 2, Row: 3, Button: MouseRight, Modifiers: ModCtrl}, x10, "\x1b[M\"#$"},
		{"x10 has no release", MouseEvent{Button: MouseLeft, Action: MouseRelease}, x10, ""},
		{"normal release", MouseEvent{Column: 0, Row: 0, Button: MouseLeft, Action: MouseRelease}, normal, "\x1b[M#!!"},
		{"normal ctrl press", MouseEvent{Button: MouseMiddle, Modifiers: ModCtrl}, normal, "\x1b[M1!!"},
		{"normal ignores motion", MouseEvent{Button: MouseLeft, Action: MouseMotion}, normal, ""},
		{"wheel up", MouseEvent{Column: 9, Row: 4, Button: MouseWheelUp}, sgr, "\x1b[<64;10;5M"},
		{"wheel has no release", MouseEvent{Button: MouseWheelDown, Action: MouseRelease}, sgr, ""},
		{"sgr release keeps button", MouseEvent{Column: 1, Row: 1, Button: MouseRight, Action: MouseRelease}, sgr, "\x1b[<2;2;2m"},
		{"button drag", MouseEvent{Column: 3, Row: 2, Button: MouseLeft, Action: MouseMotion, Modifiers: ModShift}, button, "\x1b[<36;4;3M"},
		{"button mode skips hover", MouseEvent{Button: MouseNone, Action: MouseMotion}, button, ""},
		{"any mode hover", MouseEvent{Column: 0, Row: 0, Button: MouseNone, Action: MouseMotion}, anyMotion, "\x1b[<35;1;1M"},
		{"default encoding limit", MouseEvent{Column: 223, Row: 0, Button: MouseLeft}, normal, ""},
		{"sgr has no limit", MouseEvent{Column: 239, Row: 119, Button: MouseLeft}, sgr, "\x1b[<0;240;120M"},
	}
	for _, c := range cases {
		if got := string(EncodeMouse(nil, c.ev, c.modes)); got != c.want {
			t.Fatalf("%s = %q, want %q", c.name, got, c.want)
		}
	}
}
