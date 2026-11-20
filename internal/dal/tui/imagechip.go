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

func (c *editor) attach(pasted string, at int, ch chip) {
	run := []rune(pasted)
	end := at + len(run)
	c.chips = append(c.chips, ch)
	n := len(c.chips)
	c.chips[n-1].label = c.chips[n-1].tag(n)
	atom := chipAtom(n - 1)
	if len(run) == 0 || end > len(c.text) || !slices.Equal(c.text[at:end], run) {
		c.insert([]rune{atom})
		return
	}
	c.text = slices.Replace(c.text, at, end, atom)
	switch {
	case c.cursor >= end:
		c.cursor += 1 - len(run)
	case c.cursor > at:
		c.cursor = at + 1
	}
}
