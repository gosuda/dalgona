//go:build gui

package composer

import (
	"strings"
	"unicode"
)

type entryKind uint8

const (
	entryMessage entryKind = iota
	entryCommand
	entryShell
)

type entry struct {
	kind entryKind
	body string
}

func parseEntry(text string) entry {
	switch {
	case strings.HasPrefix(text, "!"):
		return entry{kind: entryShell, body: text[1:]}
	case strings.HasPrefix(text, "//"):
		return entry{kind: entryMessage, body: text[1:]}
	case !strings.HasPrefix(text, "/"):
		return entry{kind: entryMessage, body: text}
	}
	word, _ := firstWord(text)
	if strings.Contains(word[1:], "/") {
		return entry{kind: entryMessage, body: text}
	}
	return entry{kind: entryCommand, body: text[1:]}
}

func firstWord(text string) (word string, rest bool) {
	end := strings.IndexFunc(text, unicode.IsSpace)
	if end < 0 {
		return text, false
	}
	return text[:end], true
}

func (e entry) commandQuery() (string, bool) {
	if e.kind != entryCommand {
		return "", false
	}
	if _, rest := firstWord(e.body); rest {
		return "", false
	}
	return e.body, true
}
