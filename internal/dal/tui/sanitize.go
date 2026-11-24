package tui

import (
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func displayText(text string) string {
	for i := 0; i < len(text); i++ {
		if b := text[i]; b == '\n' || b == '\t' || b >= ' ' && b < 0x7f {
			continue
		}
		return string(appendDisplay(nil, text))
	}
	return text
}

func appendDisplay(dst []byte, src string) []byte {
	for i := 0; i < len(src); {
		r, size := utf8.DecodeRuneInString(src[i:])
		if r == utf8.RuneError && size <= 1 {
			dst = append(dst, src[i])
			i++
			continue
		}
		if esc, ok := controlEscape(r); ok {
			dst = append(dst, esc...)
		} else {
			dst = append(dst, src[i:i+size]...)
		}
		i += size
	}
	return dst
}

func controlEscape(r rune) (string, bool) {
	switch {
	case r == '\n' || r == '\t':
		return "", false
	case r < 0x20 || r == 0x7f || r >= 0x80 && r <= 0x9f || isBidiControl(r):
		return "<U+" + hex4(r) + ">", true
	}
	return "", false
}

func isBidiControl(r rune) bool {
	switch {
	case r == 0x061c:
		return true
	case r >= 0x200e && r <= 0x200f:
		return true
	case r >= 0x202a && r <= 0x202e:
		return true
	case r >= 0x2066 && r <= 0x2069:
		return true
	}
	return false
}

func controlDisplay(cluster string) string {
	if r, size := utf8.DecodeRuneInString(cluster); size == len(cluster) {
		if esc, ok := controlEscape(r); ok {
			return esc
		}
	}
	return cluster
}

func hex4(r rune) string {
	const digits = "0123456789ABCDEF"
	return string([]byte{digits[r>>12&0xf], digits[r>>8&0xf], digits[r>>4&0xf], digits[r&0xf]})
}

func sanitizeText(text string) string {
	return string(appendSanitized(nil, []byte(text)))
}

func appendSanitized(dst, src []byte) []byte {
	for i := 0; i < len(src); {
		if src[i] == 0x1b {
			i = skipEscapeSequence(src, i)
			continue
		}
		r, size := utf8.DecodeRune(src[i:])
		switch {
		case r == utf8.RuneError && size == 1:
			dst = utf8.AppendRune(dst, utf8.RuneError)
		case keepRune(r):
			dst = append(dst, src[i:i+size]...)
		}
		i += size
	}
	return dst
}

var formatRanges = [...][2]rune{
	{0x061c, 0x061c},
	{0x200e, 0x200f},
	{0x202a, 0x202e},
	{0x2066, 0x2069},
	{0xf0000, 0xffffd},
	{0x100000, 0x10fffd},
}

func keepRune(r rune) bool {
	return !isControl(r) && !isFormatControl(r) && !isNoncharacter(r)
}

func isNoncharacter(r rune) bool {
	return r >= 0xfdd0 && r <= 0xfdef || r&0xfffe == 0xfffe
}

func isControl(r rune) bool {
	return r < 0x20 && r != '\n' && r != '\t' || r == 0x7f || r >= 0x80 && r <= 0x9f
}

func isFormatControl(r rune) bool {
	for _, span := range formatRanges {
		if r >= span[0] && r <= span[1] {
			return true
		}
	}
	return false
}

func skipEscapeSequence(text []byte, i int) int {
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

func skipCSI(text []byte, i int) int {
	for i++; i < len(text) && (text[i] < 0x40 || text[i] > 0x7e); i++ {
	}
	if i < len(text) {
		i++
	}
	return i
}

func skipStringSequence(text []byte, i int) int {
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

func skipShortEscape(text []byte, i int) int {
	for i++; i < len(text) && text[i] >= 0x20 && text[i] <= 0x2f; i++ {
	}
	return i + 1
}

func sanitizeItem(item rpc.Item) rpc.Item {
	item.Command = sanitizeText(item.Command)
	item.Cwd = sanitizeText(item.Cwd)
	item.Text = sanitizeText(item.Text)
	item.Output = sanitizeText(item.Output)
	item.Tool = sanitizeText(item.Tool)
	item.Server = sanitizeText(item.Server)
	item.Type = sanitizeText(item.Type)
	item.Status = sanitizeText(item.Status)
	if item.Changes != nil {
		changes := make([]rpc.FileChange, len(item.Changes))
		for i, change := range item.Changes {
			change.Path = sanitizeText(change.Path)
			change.Diff = sanitizeText(change.Diff)
			changes[i] = change
		}
		item.Changes = changes
	}
	return item
}
