package vtstate

import "github.com/charmbracelet/x/ansi"

type charsetName uint8

const (
	charsetASCII charsetName = iota
	charsetUK
	charsetSpecial
)

type charsets struct {
	designations [4]charsetName
	gl           uint8
	gr           uint8
	single       uint8
}

func defaultCharsets() charsets {
	return charsets{gr: 1}
}

var specialGraphics = [...]string{
	"◆", "▒", "␉", "␌", "␍", "␊", "°", "±", "␤", "␋", "┘", "┐", "┌", "└", "┼", "⎺",
	"⎻", "─", "⎼", "⎽", "├", "┤", "┴", "┬", "│", "⩽", "⩾", "π", "≠", "£", "·",
}

func (c charsets) mapped(content string, single uint8) (string, bool) {
	if len(content) != 1 {
		return content, false
	}
	b := content[0]
	set := c.designations[c.gl]
	if single == 2 || single == 3 {
		set = c.designations[single]
	}
	switch {
	case set == charsetUK && b == '#':
		return "£", true
	case set == charsetSpecial && b >= '`' && b <= '~':
		return specialGraphics[b-'`'], true
	}
	return content, false
}

var escHandlers = map[int]func(*Terminal){
	'=': func(t *Terminal) { t.modes |= modeKeypad },
	'>': func(t *Terminal) { t.modes &^= modeKeypad },
	'7': (*Terminal).saveCursor,
	'8': (*Terminal).restoreCursor,
	'D': (*Terminal).index,
	'H': (*Terminal).horizontalTabSet,
	'M': (*Terminal).reverseIndex,
	'c': (*Terminal).reset,
	'n': func(t *Terminal) { t.charsets.gl = 2 },
	'o': func(t *Terminal) { t.charsets.gl = 3 },
	'|': func(t *Terminal) { t.charsets.gr = 3 },
	'}': func(t *Terminal) { t.charsets.gr = 2 },
	'~': func(t *Terminal) { t.charsets.gr = 1 },
}

func (t *Terminal) handleEsc(cmd ansi.Cmd) {
	t.commitCluster()
	if handle, ok := escHandlers[int(cmd)]; ok {
		handle(t)
		return
	}
	t.designateCharset(cmd)
}

func (t *Terminal) designateCharset(cmd ansi.Cmd) {
	intermediate := cmd.Intermediate()
	if cmd.Prefix() != 0 || intermediate < '(' || intermediate > '+' {
		return
	}
	set := charsetASCII
	switch cmd.Final() {
	case 'A':
		set = charsetUK
	case 'B':
	case '0':
		set = charsetSpecial
	default:
		return
	}
	t.charsets.designations[intermediate-'('] = set
}
