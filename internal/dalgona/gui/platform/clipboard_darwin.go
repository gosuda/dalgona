//go:build darwin

package platform

// #cgo CFLAGS: -fobjc-arc -x objective-c
// #cgo LDFLAGS: -framework AppKit
// #include <stdlib.h>
// #include "clipboard_darwin.h"
import "C"

import (
	"context"
	"errors"
	"time"
	"unsafe"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const appKitImagePixelLimit = 32_000_000

const appKitClipboardTimeout = 10 * time.Second

var errPasteboardWriteFailed = errors.New("the clipboard did not accept the image; copy the image again")

type appKitClipboard struct{}

type appKitPaste struct {
	content ClipboardContent
	err     error
}

func NewAppKitClipboard() ClipboardReader {
	return appKitClipboard{}
}

func (appKitClipboard) ReadClipboard(ctx context.Context, kind PasteKind) (ClipboardContent, error) {
	paste, err := awaitNative(ctx, func() appKitPaste {
		content, readErr := readAppKitPasteboard(kind)
		return appKitPaste{content, readErr}
	})
	if err != nil {
		return ClipboardContent{}, err
	}
	return paste.content, paste.err
}

func NewAppKitImageClipboardOffer(ctx context.Context) clipboardimage.Offer {
	return NewPNGClipboardOffer(ctx, writeAppKitImage)
}

func writeAppKitImage(ctx context.Context, png []byte) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	status := C.dalgona_pasteboard_write_image((*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(png))), C.size_t(len(png))) //nolint:gosec // The live Go buffer holds exactly len(png) bytes.
	if status != appKitPasteboardRead {
		return errPasteboardWriteFailed
	}
	return nil
}

func readAppKitPasteboard(kind PasteKind) (ClipboardContent, error) {
	var change C.long
	format, err := ChoosePaste(ClipboardFormats(C.dalgona_pasteboard_formats(&change)), kind)
	if err != nil {
		return ClipboardContent{}, err
	}
	var transfer C.dalgona_pasteboard_bytes
	status := C.dalgona_pasteboard_read(C.uint8_t(format), change, C.size_t(transferLimit), C.uint64_t(appKitImagePixelLimit), &transfer) //nolint:gocritic // cgo writes the pointer check of this call as a comparison of two equal constants
	defer C.free(unsafe.Pointer(transfer.data))                                                                                           //nolint:gosec // dalgona_pasteboard_read allocates the transfer with malloc
	if readErr := appKitPasteboardError(int(status)); readErr != nil {
		return ClipboardContent{}, readErr
	}
	data := unsafe.Slice((*byte)(unsafe.Pointer(transfer.data)), int(transfer.length)) //nolint:gosec // the transfer holds exactly length bytes allocated by dalgona_pasteboard_read
	return decodeAppKitPasteboard(format, data)
}
