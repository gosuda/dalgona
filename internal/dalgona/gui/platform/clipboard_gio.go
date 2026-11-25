//go:build gui

package platform

import (
	"fmt"
	"io"
	"strings"

	"gioui.org/io/clipboard"
	"gioui.org/io/event"
	"gioui.org/io/input"
	"gioui.org/io/transfer"
)

const textMIME = "application/text"

func WriteText(src input.Source, text string) error {
	if err := CheckCopy(text); err != nil {
		return err
	}
	src.Execute(clipboard.WriteCmd{Type: textMIME, Data: io.NopCloser(strings.NewReader(text))})
	return nil
}

func PasteFilter(target event.Tag) transfer.TargetFilter {
	return transfer.TargetFilter{Target: target, Type: textMIME}
}

func ReadPaste(e transfer.DataEvent) (string, error) {
	if e.Type != textMIME {
		return "", ErrUnsupportedPaste
	}
	stream := e.Open()
	text, err := ReadText(stream)
	if closeErr := stream.Close(); err == nil && closeErr != nil {
		return "", fmt.Errorf("close the clipboard transfer: %w", closeErr)
	}
	return text, err
}
