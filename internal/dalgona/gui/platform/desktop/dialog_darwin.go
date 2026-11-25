//go:build gui && darwin

package desktop

// #cgo CFLAGS: -fobjc-arc -x objective-c
// #cgo LDFLAGS: -framework AppKit -framework UniformTypeIdentifiers
// #include "dialog_darwin.h"
import "C"

import (
	"context"
	"fmt"
	"math"
	"strings"
	"unsafe"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type sheetParent struct {
	handle C.uintptr_t
}

type sheet struct {
	handle C.uintptr_t
}

func newSheetParent(view uintptr) sheetParent {
	return sheetParent{C.dalgona_dialog_parent(C.uintptr_t(view))}
}

func (p sheetParent) release() {
	C.dalgona_dialog_parent_release(p.handle)
}

func beginSheet(parent sheetParent, req platform.DialogRequest) (sheet, error) {
	if parent.handle == 0 {
		return sheet{}, ErrNoView
	}
	var extensions []string
	for _, f := range req.Filters {
		extensions = append(extensions, f.Extensions...)
	}
	title, titleLength := cBytes(req.Title)
	name, nameLength := cBytes(req.SuggestedName)
	types, typesLength := cBytes(strings.Join(extensions, "\x00"))
	return sheet{C.dalgona_dialog_begin(parent.handle, sheetKind(req.Kind), title, titleLength, name, nameLength, types, typesLength)}, nil
}

func sheetKind(kind platform.DialogKind) C.uint8_t {
	switch kind {
	case platform.DialogChooseFolder:
		return C.DalgonaDialogFolder
	case platform.DialogSaveFile:
		return C.DalgonaDialogSave
	case platform.DialogOpenFiles:
	}
	return C.DalgonaDialogOpen
}

func cBytes(text string) (*C.uint8_t, C.size_t) {
	return (*C.uint8_t)(unsafe.Pointer(unsafe.StringData(text))), C.size_t(len(text)) //nolint:gosec // the bridge copies the bytes before it returns
}

func (s sheet) await(ctx context.Context) (platform.DialogResult, error) {
	defer C.dalgona_dialog_release(s.handle)
	cancelled := make(chan struct{})
	stop := context.AfterFunc(ctx, func() {
		defer close(cancelled)
		C.dalgona_dialog_cancel(s.handle)
	})
	var paths *C.uint8_t
	var length C.size_t
	status := C.dalgona_dialog_wait(s.handle, &paths, &length) //nolint:gocritic // cgo writes the pointer check of this call as a comparison of two equal constants
	if !stop() {
		<-cancelled
	}
	if err := ctx.Err(); err != nil {
		return platform.DialogResult{}, err
	}
	return sheetResult(status, paths, length)
}

func sheetResult(status C.int, paths *C.uint8_t, length C.size_t) (platform.DialogResult, error) {
	switch status {
	case C.DalgonaDialogChosen:
		if length == 0 || length > math.MaxInt32 {
			return platform.DialogResult{}, platform.ErrNotLocalFile
		}
		files, err := splitPaths(C.GoBytes(unsafe.Pointer(paths), C.int(length))) //nolint:gosec // the dialog owns length bytes at paths until it is released
		if err != nil {
			return platform.DialogResult{}, err
		}
		return platform.DialogResult{Files: files}, nil
	case C.DalgonaDialogCancelled:
		return platform.DialogResult{Cancelled: true}, nil
	case C.DalgonaDialogNoWindow:
		return platform.DialogResult{}, ErrNoView
	case C.DalgonaDialogRemote:
		return platform.DialogResult{}, platform.ErrNotLocalFile
	}
	return platform.DialogResult{}, fmt.Errorf("the macOS file dialog ended with unknown status %d; try again", int(status))
}
