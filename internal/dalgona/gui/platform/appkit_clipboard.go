package platform

import (
	"bytes"
	"errors"
)

const (
	appKitPasteboardRead = iota
	appKitPasteboardChanged
	appKitPasteboardTooLarge
	appKitPasteboardUnsupported
	appKitPasteboardNotLocal
	appKitPasteboardImageFailed
	appKitPasteboardTooManyPixels
	appKitPasteboardOutOfMemory
)

var (
	errPasteboardChanged       = errors.New("the clipboard changed while it was read; paste again")
	errPasteboardImageFailed   = errors.New("the clipboard image could not be converted to PNG; copy the image again as a PNG or TIFF image")
	errPasteboardTooManyPixels = errors.New("the clipboard image has more than 32000000 pixels; copy a smaller image")
	errPasteboardOutOfMemory   = errors.New("the clipboard content does not fit in memory; close other applications, then paste again")
)

var appKitPasteboardErrors = [...]error{
	appKitPasteboardRead:          nil,
	appKitPasteboardChanged:       errPasteboardChanged,
	appKitPasteboardTooLarge:      ErrTransferTooLarge,
	appKitPasteboardUnsupported:   ErrUnsupportedPaste,
	appKitPasteboardNotLocal:      ErrNotLocalFile,
	appKitPasteboardImageFailed:   errPasteboardImageFailed,
	appKitPasteboardTooManyPixels: errPasteboardTooManyPixels,
	appKitPasteboardOutOfMemory:   errPasteboardOutOfMemory,
}

func appKitPasteboardError(status int) error {
	if status < 0 || status >= len(appKitPasteboardErrors) {
		return ErrUnsupportedPaste
	}
	return appKitPasteboardErrors[status]
}

func decodeAppKitPasteboard(format ClipboardFormats, data []byte) (ClipboardContent, error) {
	if len(data) > transferLimit {
		return ClipboardContent{}, ErrTransferTooLarge
	}
	switch format {
	case ClipboardText:
		text, err := ReadText(bytes.NewReader(data))
		if err != nil {
			return ClipboardContent{}, err
		}
		return ClipboardContent{Text: text}, nil
	case ClipboardImage:
		if len(data) == 0 {
			return ClipboardContent{}, ErrUnsupportedPaste
		}
		return ClipboardContent{Image: bytes.Clone(data), ImageType: imagePNGMIME}, nil
	case ClipboardFiles:
		files, err := ParseURIList(string(data))
		if err != nil {
			return ClipboardContent{}, err
		}
		return ClipboardContent{Files: files}, nil
	}
	return ClipboardContent{}, ErrUnsupportedPaste
}
