package terminal

import (
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

type Key uint8

const (
	KeyUp Key = iota + 1
	KeyDown
	KeyRight
	KeyLeft
	KeyHome
	KeyEnd
	KeyInsert
	KeyDelete
	KeyPageUp
	KeyPageDown
	KeyF1
	KeyF2
	KeyF3
	KeyF4
	KeyF5
	KeyF6
	KeyF7
	KeyF8
	KeyF9
	KeyF10
	KeyF11
	KeyF12
	KeyEnter
	KeyTab
	KeyBackspace
	KeyEscape
)

type Modifiers uint8

const (
	ModShift Modifiers = 1 << iota
	ModAlt
	ModCtrl
)

func (m Modifiers) parameter() int {
	return 1 + int(m&(ModShift|ModAlt|ModCtrl))
}

type keyCode struct {
	final  byte
	number int
	cursor bool
}

var keyCodes = map[Key]keyCode{
	KeyUp:       {final: 'A', cursor: true},
	KeyDown:     {final: 'B', cursor: true},
	KeyRight:    {final: 'C', cursor: true},
	KeyLeft:     {final: 'D', cursor: true},
	KeyHome:     {final: 'H', cursor: true},
	KeyEnd:      {final: 'F', cursor: true},
	KeyInsert:   {final: '~', number: 2},
	KeyDelete:   {final: '~', number: 3},
	KeyPageUp:   {final: '~', number: 5},
	KeyPageDown: {final: '~', number: 6},
	KeyF1:       {final: 'P'},
	KeyF2:       {final: 'Q'},
	KeyF3:       {final: 'R'},
	KeyF4:       {final: 'S'},
	KeyF5:       {final: '~', number: 15},
	KeyF6:       {final: '~', number: 17},
	KeyF7:       {final: '~', number: 18},
	KeyF8:       {final: '~', number: 19},
	KeyF9:       {final: '~', number: 20},
	KeyF10:      {final: '~', number: 21},
	KeyF11:      {final: '~', number: 23},
	KeyF12:      {final: '~', number: 24},
}

func EncodeKey(dst []byte, key Key, mods Modifiers, modes vtstate.InputModes) []byte {
	if code, ok := keyCodes[key]; ok {
		return code.encode(dst, mods, modes)
	}
	if mods&ModAlt != 0 {
		dst = append(dst, 0x1b)
	}
	return appendEditingKey(dst, key, mods, modes)
}

func appendEditingKey(dst []byte, key Key, mods Modifiers, modes vtstate.InputModes) []byte {
	switch key {
	case KeyEnter:
		if modes.LineFeedNewLine {
			return append(dst, '\r', '\n')
		}
		return append(dst, '\r')
	case KeyTab:
		if mods&ModShift != 0 {
			return append(dst, "\x1b[Z"...)
		}
		return append(dst, '\t')
	case KeyBackspace:
		if mods&ModCtrl != 0 {
			return append(dst, 0x08)
		}
		return append(dst, 0x7f)
	case KeyEscape:
		return append(dst, 0x1b)
	case KeyUp, KeyDown, KeyRight, KeyLeft, KeyHome, KeyEnd, KeyInsert, KeyDelete, KeyPageUp, KeyPageDown, KeyF1, KeyF2, KeyF3, KeyF4, KeyF5, KeyF6, KeyF7, KeyF8, KeyF9, KeyF10, KeyF11, KeyF12:
		return dst
	}
	return dst
}

func (c keyCode) encode(dst []byte, mods Modifiers, modes vtstate.InputModes) []byte {
	modified := mods&(ModShift|ModAlt|ModCtrl) != 0
	switch {
	case c.final == '~':
		dst = append(dst, "\x1b["...)
		dst = strconv.AppendInt(dst, int64(c.number), 10)
		if modified {
			dst = append(dst, ';')
			dst = strconv.AppendInt(dst, int64(mods.parameter()), 10)
		}
	case modified:
		dst = append(dst, "\x1b[1;"...)
		dst = strconv.AppendInt(dst, int64(mods.parameter()), 10)
	case !c.cursor || modes.ApplicationCursor:
		dst = append(dst, 0x1b, 'O')
	default:
		dst = append(dst, 0x1b, '[')
	}
	return append(dst, c.final)
}

func EncodeText(dst []byte, text string, mods Modifiers) []byte {
	if mods&ModAlt != 0 {
		dst = append(dst, 0x1b)
	}
	if mods&ModCtrl != 0 && len(text) == 1 {
		if control, ok := controlCharacter(text[0]); ok {
			return append(dst, control)
		}
	}
	return append(dst, text...)
}

func controlCharacter(b byte) (byte, bool) {
	switch {
	case b >= 'a' && b <= 'z':
		return b - 'a' + 1, true
	case b >= '@' && b <= '_':
		return b - '@', true
	case b == ' ':
		return 0, true
	case b == '?':
		return 0x7f, true
	}
	return 0, false
}

const (
	pasteStart = "\x1b[200~"
	pasteEnd   = "\x1b[201~"
)

func EncodePaste(dst []byte, text string, modes vtstate.InputModes) []byte {
	text = strings.ReplaceAll(text, "\r\n", "\r")
	text = strings.ReplaceAll(text, "\n", "\r")
	if !modes.BracketedPaste {
		return append(dst, text...)
	}
	dst = append(dst, pasteStart...)
	dst = append(dst, strings.ReplaceAll(text, pasteEnd, "")...)
	return append(dst, pasteEnd...)
}

func EncodeFocus(dst []byte, focused bool, modes vtstate.InputModes) []byte {
	switch {
	case !modes.FocusEvents:
		return dst
	case focused:
		return append(dst, "\x1b[I"...)
	}
	return append(dst, "\x1b[O"...)
}

type MouseButton uint8

const (
	MouseNone MouseButton = iota
	MouseLeft
	MouseMiddle
	MouseRight
	MouseWheelUp
	MouseWheelDown
)

type MouseAction uint8

const (
	MousePress MouseAction = iota
	MouseRelease
	MouseMotion
)

type MouseEvent struct {
	Column    int
	Row       int
	Button    MouseButton
	Action    MouseAction
	Modifiers Modifiers
}

func EncodeMouse(dst []byte, ev MouseEvent, modes vtstate.InputModes) []byte {
	if !reportsMouse(ev, modes) {
		return dst
	}
	if modes.MouseSGR {
		return appendSGRMouse(dst, mouseCode(ev, modes), ev)
	}
	return appendX10Mouse(dst, ev, modes)
}

func mouseCode(ev MouseEvent, modes vtstate.InputModes) int {
	code := buttonCode(ev)
	if !modes.MouseX10 || modes.MouseNormal || modes.MouseButton || modes.MouseAny {
		code += modifierCode(ev.Modifiers)
	}
	return code
}

func appendX10Mouse(dst []byte, ev MouseEvent, modes vtstate.InputModes) []byte {
	code := mouseCode(ev, modes)
	if ev.Action == MouseRelease {
		code = 3 + modifierCode(ev.Modifiers)
	}
	code += 32
	x, y := 32+ev.Column+1, 32+ev.Row+1
	if code < 0 || code > 255 || x < 0 || x > 255 || y < 0 || y > 255 {
		return dst
	}
	return append(dst, 0x1b, '[', 'M', byte(code), byte(x), byte(y))
}

func appendSGRMouse(dst []byte, code int, ev MouseEvent) []byte {
	dst = append(dst, "\x1b[<"...)
	dst = strconv.AppendInt(dst, int64(code), 10)
	dst = append(dst, ';')
	dst = strconv.AppendInt(dst, int64(ev.Column+1), 10)
	dst = append(dst, ';')
	dst = strconv.AppendInt(dst, int64(ev.Row+1), 10)
	if ev.Action == MouseRelease {
		return append(dst, 'm')
	}
	return append(dst, 'M')
}

func reportsMouse(ev MouseEvent, modes vtstate.InputModes) bool {
	tracking := modes.MouseNormal || modes.MouseButton || modes.MouseAny
	wheel := ev.Button >= MouseWheelUp
	switch {
	case ev.Action == MousePress:
		return tracking || modes.MouseX10
	case ev.Action == MouseRelease:
		return tracking && !wheel
	case ev.Button == MouseNone:
		return modes.MouseAny
	}
	return modes.MouseButton || modes.MouseAny
}

func buttonCode(ev MouseEvent) int {
	codes := [...]int{MouseNone: 3, MouseLeft: 0, MouseMiddle: 1, MouseRight: 2, MouseWheelUp: 64, MouseWheelDown: 65}
	code := codes[ev.Button]
	if ev.Action == MouseMotion {
		code += 32
	}
	return code
}

func modifierCode(m Modifiers) int {
	code := 0
	if m&ModShift != 0 {
		code += 4
	}
	if m&ModAlt != 0 {
		code += 8
	}
	if m&ModCtrl != 0 {
		code += 16
	}
	return code
}
