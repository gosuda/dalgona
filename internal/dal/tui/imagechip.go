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
	for _, ch := range c.chips {
		if ch.image != "" {
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

func (c editor) holds(run []rune, at int) bool {
	end := at + len(run)
	if len(run) == 0 || end > len(c.text) || !slices.Equal(c.text[at:end], run) {
		return false
	}
	return c.chipIndex(at+1) == c.chipIndex(end)
}

func (c *editor) attach(pasted string, at int, ch chip) {
	run := []rune(pasted)
	if !c.holds(run, at) {
		c.addChip(ch)
		return
	}
	end := at + len(run)
	slot, resident := c.chipIndex(at+1), c.chipsAt(at)
	c.cutText(at, end)
	ch.at = at
	c.chips = slices.Concat(c.chips[:slot], []chip{ch}, c.chips[slot:])
	switch {
	case c.cursor >= end:
		c.cursor -= len(run)
		if c.cursor == at {
			c.lead += resident + 1
		}
	case c.cursor > at:
		c.cursor, c.lead = at, resident+1
	}
	c.verbatimText = nil
	c.prune()
}
