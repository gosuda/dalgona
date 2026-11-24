package platform

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const transferLimit = 32 << 20

const imagePNGMIME = "image/png"

var (
	ErrTransferTooLarge = errors.New("the transfer exceeds 32 MiB; transfer less content")
	ErrUnsupportedPaste = errors.New("the clipboard holds no text, image, or file in a supported format; copy the content again as text, an image, or a file")
	ErrPasteStale       = errors.New("the paste target changed before the clipboard answered; paste again")
)

type PasteKind uint8

const (
	PasteOrdinary PasteKind = iota + 1
	PasteImage
)

type ClipboardFormats uint8

const (
	ClipboardText ClipboardFormats = 1 << iota
	ClipboardImage
	ClipboardFiles
)

type ClipboardContent struct {
	Text      string
	Image     []byte
	ImageType string
	Files     []string
}

type ClipboardReader interface {
	ReadClipboard(ctx context.Context, kind PasteKind) (ClipboardContent, error)
}

func ChoosePaste(available ClipboardFormats, kind PasteKind) (ClipboardFormats, error) {
	var order []ClipboardFormats
	switch kind {
	case PasteOrdinary:
		order = []ClipboardFormats{ClipboardFiles, ClipboardText, ClipboardImage}
	case PasteImage:
		order = []ClipboardFormats{ClipboardImage, ClipboardFiles, ClipboardText}
	}
	for _, format := range order {
		if available&format != 0 {
			return format, nil
		}
	}
	return 0, ErrUnsupportedPaste
}

func ReadText(r io.Reader) (string, error) {
	data, err := io.ReadAll(io.LimitReader(r, transferLimit+1))
	if err != nil {
		return "", fmt.Errorf("read the transferred text: %w", err)
	}
	if len(data) > transferLimit {
		return "", ErrTransferTooLarge
	}
	text, err := inputtext.DecodeUTF8(data)
	if err != nil {
		return "", err
	}
	if strings.IndexByte(text, 0) >= 0 {
		return "", inputtext.DisallowedControl
	}
	return text, nil
}

func CheckCopy(text string) error {
	switch {
	case len(text) > transferLimit:
		return ErrTransferTooLarge
	case !utf8.ValidString(text):
		return inputtext.InvalidEncoding
	case strings.IndexByte(text, 0) >= 0:
		return inputtext.DisallowedControl
	}
	return nil
}

type PasteTarget struct {
	Window    Generation
	Endpoint  string
	Thread    string
	Field     string
	Revision  uint64
	Selection inputtext.Range
}

func (t PasteTarget) Check(current PasteTarget) error {
	if t != current {
		return ErrPasteStale
	}
	return nil
}
