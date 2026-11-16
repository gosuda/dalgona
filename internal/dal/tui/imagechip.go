package tui

import (
	"fmt"
	"slices"
)

func (ch chip) tag(number int) string {
	if ch.image != "" {
		return fmt.Sprintf("[Image #%d %s]", number, ch.size)
	}
	return chipLabel(number, ch.text)
}

func (c editor) images() []chip {
	var out []chip
	for _, r := range c.text {
		if ch, ok := c.chipOf(r); ok && ch.image != "" {
			out = append(out, ch)
		}
	}
	return out
}

func (c editor) imageBytes() int {
	total := 0
	for _, ch := range c.images() {
		total += ch.bytes
	}
	return total
}

func indexRun(text, run []rune) int {
	for i := 0; i+len(run) <= len(text); i++ {
		if slices.Equal(text[i:i+len(run)], run) {
			return i
		}
	}
	return -1
}

func (c *editor) attach(pasted string, ch chip) bool {
	run := []rune(pasted)
	at := -1
	if pasted != "" {
		if at = indexRun(c.text, run); at < 0 {
			return false
		}
	}
	c.chips = append(c.chips, ch)
	n := len(c.chips)
	c.chips[n-1].label = c.chips[n-1].tag(n)
	atom := chipAtom(n - 1)
	if at < 0 {
		c.insert([]rune{atom})
		return true
	}
	end := at + len(run)
	c.text = slices.Replace(c.text, at, end, atom)
	switch {
	case c.cursor >= end:
		c.cursor += 1 - len(run)
	case c.cursor > at:
		c.cursor = at + 1
	}
	return true
}
