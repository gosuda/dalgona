package tui

import (
	"errors"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

var fieldPolicy = inputtext.Policy{Lines: inputtext.SingleLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: pasteLimit}}

var composerPolicy = inputtext.Policy{Lines: inputtext.MultiLine, Limit: inputtext.Limit{Unit: inputtext.Bytes, Max: 15 << 20}}

func foldValidPaste(text string) (string, bool) {
	next, err := inputtext.Draft{}.Replace(composerPolicy, text)
	if err != nil {
		return "", false
	}
	return next.Text, true
}

func composerPasteRefusal(text string) string {
	_, err := inputtext.Draft{}.Replace(composerPolicy, text)
	return pasteRefusal(err)
}

func (f textField) pasteRefusal(text string) string {
	_, err := f.draft().Replace(fieldPolicy, text)
	return pasteRefusal(err)
}

func pasteRefusal(err error) string {
	switch {
	case err == nil:
		return ""
	case errors.Is(err, inputtext.InvalidEncoding):
		return "Paste holds invalid UTF-8. Nothing was pasted."
	case errors.Is(err, inputtext.DisallowedControl):
		return "Paste holds a NUL byte. Nothing was pasted."
	case errors.Is(err, inputtext.LineBreakNotAllowed):
		return "Paste holds a line break, but this field takes one line. Nothing was pasted."
	}
	return "Paste makes this field longer than 1 MiB. Nothing was pasted."
}
