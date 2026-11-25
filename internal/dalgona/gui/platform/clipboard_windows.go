//go:build windows

package platform

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"image/png"
	"runtime"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const (
	clipboardFormatDIB         = 8
	clipboardFormatUnicodeText = 13
	clipboardFormatHDrop       = 15
	clipboardFormatDIBV5       = 17
	clipboardUintBits          = 0xFFFFFFFF
	dragQueryFileCount         = 0xFFFFFFFF
	clipboardRetryFirst        = time.Millisecond
	clipboardRetryLongest      = 50 * time.Millisecond
	clipboardGlobalMoveable    = 0x0002
	clipboardWriteTimeout      = 5 * time.Second
)

var (
	clipboardUser32                = windows.NewLazySystemDLL("user32.dll")
	clipboardKernel32              = windows.NewLazySystemDLL("kernel32.dll")
	clipboardNtdll                 = windows.NewLazySystemDLL("ntdll.dll")
	clipboardShell32               = windows.NewLazySystemDLL("shell32.dll")
	procOpenClipboard              = clipboardUser32.NewProc("OpenClipboard")
	procCloseClipboard             = clipboardUser32.NewProc("CloseClipboard")
	procEmptyClipboard             = clipboardUser32.NewProc("EmptyClipboard")
	procSetClipboardData           = clipboardUser32.NewProc("SetClipboardData")
	procGetClipboardData           = clipboardUser32.NewProc("GetClipboardData")
	procIsClipboardFormatAvailable = clipboardUser32.NewProc("IsClipboardFormatAvailable")
	procRegisterClipboardFormatW   = clipboardUser32.NewProc("RegisterClipboardFormatW")
	procClipboardGlobalAlloc       = clipboardKernel32.NewProc("GlobalAlloc")
	procClipboardGlobalFree        = clipboardKernel32.NewProc("GlobalFree")
	procClipboardGlobalLock        = clipboardKernel32.NewProc("GlobalLock")
	procClipboardGlobalUnlock      = clipboardKernel32.NewProc("GlobalUnlock")
	procClipboardGlobalSize        = clipboardKernel32.NewProc("GlobalSize")
	procClipboardMoveMemory        = clipboardNtdll.NewProc("RtlMoveMemory")
	procClipboardDragQueryFileW    = clipboardShell32.NewProc("DragQueryFileW")
	clipboardPNGFormatName         = [...]uint16{'P', 'N', 'G', 0}
	errClipboardFileName           = errors.New("the copied file list holds an empty or changing file name; copy the files again")
)

type windowsClipboard struct{}

type windowsClipboardCopy struct {
	format      ClipboardFormats
	imageFormat uintptr
	image       []byte
	units       []uint16
	files       []string
}

func NewWindowsClipboard() ClipboardReader {
	return windowsClipboard{}
}

func (windowsClipboard) ReadClipboard(ctx context.Context, kind PasteKind) (ClipboardContent, error) {
	copied, err := copyClipboard(ctx, kind)
	if err != nil {
		return ClipboardContent{}, err
	}
	return copied.content()
}

func copyClipboard(ctx context.Context, kind PasteKind) (copied windowsClipboardCopy, err error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	if openErr := openClipboard(ctx); openErr != nil {
		return windowsClipboardCopy{}, openErr
	}
	defer func() {
		closed, _, closeErr := procCloseClipboard.Call()
		if closed&clipboardUintBits == 0 && err == nil {
			copied, err = windowsClipboardCopy{}, fmt.Errorf("close the Windows clipboard: %w; paste again", closeErr)
		}
	}()
	return copyOpenClipboard(kind)
}

func openClipboard(ctx context.Context) error {
	retry := time.NewTimer(clipboardRetryFirst)
	defer retry.Stop()
	for delay := clipboardRetryFirst; ; {
		opened, _, openErr := procOpenClipboard.Call(0)
		if opened&clipboardUintBits != 0 {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("open the Windows clipboard: %w; another application kept it open (%w), so paste again when it finishes", context.Cause(ctx), openErr)
		case <-retry.C:
		}
		delay = min(2*delay, clipboardRetryLongest)
		retry.Reset(delay)
	}
}

func copyOpenClipboard(kind PasteKind) (windowsClipboardCopy, error) {
	imageFormat, err := availableImageFormat()
	if err != nil {
		return windowsClipboardCopy{}, err
	}
	format, err := ChoosePaste(availableClipboardFormats(imageFormat), kind)
	if err != nil {
		return windowsClipboardCopy{}, err
	}
	copied := windowsClipboardCopy{format: format, imageFormat: imageFormat}
	switch format {
	case ClipboardText:
		copied.units, err = copyUnicodeText()
	case ClipboardImage:
		copied.image, err = copyClipboardBytes(imageFormat)
	case ClipboardFiles:
		copied.files, err = copyClipboardFiles()
	}
	if err != nil {
		return windowsClipboardCopy{}, err
	}
	return copied, nil
}

func availableImageFormat() (uintptr, error) {
	registered, _, err := procRegisterClipboardFormatW.Call(uintptr(unsafe.Pointer(&clipboardPNGFormatName[0]))) //nolint:gosec // RegisterClipboardFormatW reads the NUL-terminated format name through a raw pointer
	pngFormat := registered & clipboardUintBits
	if pngFormat == 0 {
		return 0, fmt.Errorf("register the PNG clipboard format: %w; restart dalgona and paste again", err)
	}
	for _, format := range [...]uintptr{pngFormat, clipboardFormatDIBV5, clipboardFormatDIB} {
		if clipboardHolds(format) {
			return format, nil
		}
	}
	return 0, nil
}

func availableClipboardFormats(imageFormat uintptr) ClipboardFormats {
	var available ClipboardFormats
	if clipboardHolds(clipboardFormatUnicodeText) {
		available |= ClipboardText
	}
	if imageFormat != 0 {
		available |= ClipboardImage
	}
	if clipboardHolds(clipboardFormatHDrop) {
		available |= ClipboardFiles
	}
	return available
}

func clipboardHolds(format uintptr) bool {
	available, _, _ := procIsClipboardFormatAvailable.Call(format)
	return available&clipboardUintBits != 0
}

func clipboardData(format uintptr) (handle, size uintptr, err error) {
	handle, _, err = procGetClipboardData.Call(format)
	if handle == 0 {
		return 0, 0, fmt.Errorf("read clipboard format %d: %w; copy the content again", format, err)
	}
	size, _, err = procClipboardGlobalSize.Call(handle)
	if size == 0 {
		return 0, 0, fmt.Errorf("measure clipboard format %d: %w; copy the content again", format, err)
	}
	return handle, size, nil
}

func copyUnicodeText() ([]uint16, error) {
	handle, size, err := clipboardData(clipboardFormatUnicodeText)
	if err != nil {
		return nil, err
	}
	count, err := unicodeTextUnits(size)
	if err != nil {
		return nil, err
	}
	units := make([]uint16, count)
	if err := copyGlobal(handle, unsafe.Pointer(unsafe.SliceData(units)), size); err != nil { //nolint:gosec // units holds exactly size bytes for the copy
		return nil, err
	}
	return units, nil
}

func copyClipboardBytes(format uintptr) ([]byte, error) {
	handle, size, err := clipboardData(format)
	if err != nil {
		return nil, err
	}
	if size > transferLimit {
		return nil, ErrTransferTooLarge
	}
	data := make([]byte, size)
	if err := copyGlobal(handle, unsafe.Pointer(unsafe.SliceData(data)), size); err != nil { //nolint:gosec // data holds exactly size bytes for the copy
		return nil, err
	}
	return data, nil
}

func copyGlobal(handle uintptr, target unsafe.Pointer, size uintptr) error {
	source, _, err := procClipboardGlobalLock.Call(handle)
	if source == 0 {
		return fmt.Errorf("lock the clipboard data: %w; copy the content again", err)
	}
	_, _, _ = procClipboardMoveMemory.Call(uintptr(target), source, size)
	_, _, _ = procClipboardGlobalUnlock.Call(handle)
	return nil
}

func copyClipboardFiles() ([]string, error) {
	handle, size, err := clipboardData(clipboardFormatHDrop)
	if err != nil {
		return nil, err
	}
	if size > transferLimit {
		return nil, ErrTransferTooLarge
	}
	count, _, _ := procClipboardDragQueryFileW.Call(handle, dragQueryFileCount, 0, 0)
	count &= clipboardUintBits
	if count == 0 {
		return nil, ErrUnsupportedPaste
	}
	files := make([]string, 0, count)
	for index := range count {
		file, err := dragQueryFile(handle, index)
		if err != nil {
			return nil, err
		}
		files = append(files, file)
	}
	return files, nil
}

func dragQueryFile(handle, index uintptr) (string, error) {
	length, _, _ := procClipboardDragQueryFileW.Call(handle, index, 0, 0)
	length &= clipboardUintBits
	if length == 0 {
		return "", errClipboardFileName
	}
	name := make([]uint16, length+1)
	copied, _, _ := procClipboardDragQueryFileW.Call(handle, index, uintptr(unsafe.Pointer(unsafe.SliceData(name))), length+1) //nolint:gosec // DragQueryFileW writes at most length+1 units into name
	if copied&clipboardUintBits != length {
		return "", errClipboardFileName
	}
	file, err := textinput.DecodeUTF16(name[:length])
	if err != nil {
		return "", fmt.Errorf("read copied file %d: %w; rename the file to valid Unicode and copy it again", index+1, err)
	}
	return file, nil
}

func (c windowsClipboardCopy) content() (ClipboardContent, error) {
	switch c.format {
	case ClipboardText:
		text, err := decodeUnicodeText(c.units)
		if err != nil {
			return ClipboardContent{}, err
		}
		return ClipboardContent{Text: text}, nil
	case ClipboardImage:
		image, err := clipboardPNG(c.imageFormat, c.image)
		if err != nil {
			return ClipboardContent{}, err
		}
		return ClipboardContent{Image: image, ImageType: "image/png"}, nil
	case ClipboardFiles:
		return ClipboardContent{Files: c.files}, nil
	}
	return ClipboardContent{}, ErrUnsupportedPaste
}

func clipboardPNG(format uintptr, data []byte) ([]byte, error) {
	if format == clipboardFormatDIBV5 || format == clipboardFormatDIB {
		return pngFromDIB(data)
	}
	if _, err := png.DecodeConfig(bytes.NewReader(data)); err != nil {
		return nil, fmt.Errorf("read the copied PNG image: %w; copy the image again", err)
	}
	return data, nil
}

func pngClipboardFormat() (uintptr, error) {
	registered, _, err := procRegisterClipboardFormatW.Call(uintptr(unsafe.Pointer(&clipboardPNGFormatName[0]))) //nolint:gosec // RegisterClipboardFormatW reads the NUL-terminated format name through a raw pointer
	format := registered & clipboardUintBits
	if format == 0 {
		return 0, fmt.Errorf("register the PNG clipboard format: %w; restart dalgona and copy the image again", err)
	}
	return format, nil
}
