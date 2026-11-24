//go:build gui

package composer

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type mention struct {
	span  inputtext.Range
	query string
}

func mentionAt(text string, caret int) (mention, bool) {
	if caret < 0 || caret > len(text) || !utf8.ValidString(text) {
		return mention{}, false
	}
	start := strings.LastIndexFunc(text[:caret], unicode.IsSpace)
	if start < 0 {
		start = 0
	} else {
		_, size := utf8.DecodeRuneInString(text[start:])
		start += size
	}
	end := strings.IndexFunc(text[caret:], unicode.IsSpace)
	if end < 0 {
		end = len(text)
	} else {
		end += caret
	}
	if start >= end || text[start] != '@' {
		return mention{}, false
	}
	return mention{span: inputtext.Range{Start: start, End: end}, query: text[start+1 : end]}, true
}

func insertedPath(path string) string {
	return "@" + path + " "
}
