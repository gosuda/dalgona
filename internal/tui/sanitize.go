package tui

import (
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/rpc"
)

func sanitizeText(text string) string {
	var out strings.Builder
	out.Grow(len(text))
	for i := 0; i < len(text); {
		c := text[i]
		if c == 0x1b {
			i = skipEscapeSequence(text, i)
			continue
		}
		r, size := utf8.DecodeRuneInString(text[i:])
		if r < 0x20 && r != '\n' && r != '\t' || r == 0x7f || r >= 0x80 && r <= 0x9f {
			i += size
			continue
		}
		out.WriteString(text[i : i+size])
		i += size
	}
	return out.String()
}

func skipEscapeSequence(text string, i int) int {
	i++
	if i >= len(text) {
		return i
	}
	switch text[i] {
	case '[':
		return skipCSI(text, i)
	case ']', 'P', 'X', '^', '_':
		return skipStringSequence(text, i)
	default:
		return skipShortEscape(text, i)
	}
}

func skipCSI(text string, i int) int {
	for i++; i < len(text) && (text[i] < 0x40 || text[i] > 0x7e); i++ {
	}
	if i < len(text) {
		i++
	}
	return i
}

func skipStringSequence(text string, i int) int {
	for i++; i < len(text); i++ {
		switch {
		case text[i] == 0x07:
			return i + 1
		case text[i] == 0x1b && i+1 < len(text) && text[i+1] == '\\':
			return i + 2
		}
	}
	return i
}

func skipShortEscape(text string, i int) int {
	for i++; i < len(text) && text[i] >= 0x20 && text[i] <= 0x2f; i++ {
	}
	return i + 1
}

func sanitizeItem(item rpc.Item) rpc.Item {
	item.Command = sanitizeText(item.Command)
	item.Text = sanitizeText(item.Text)
	item.Output = sanitizeText(item.Output)
	item.Tool = sanitizeText(item.Tool)
	item.Server = sanitizeText(item.Server)
	item.Type = sanitizeText(item.Type)
	item.Status = sanitizeText(item.Status)
	return item
}
