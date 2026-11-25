//go:build windows

package platform

import (
	"context"
	"errors"
	"fmt"
	"io"
	"runtime"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

type windowsClipboardOffer struct {
	ctx      context.Context
	owner    uintptr
	png, dib clipboardGlobal
}

func NewWindowsClipboardOffer(ctx context.Context, owner uintptr) clipboardimage.Offer {
	return &windowsClipboardOffer{ctx: ctx, owner: owner}
}

func (*windowsClipboardOffer) NeedsDIB() bool { return true }

func (w *windowsClipboardOffer) Stage(spec clipboardimage.Spec) (io.Writer, io.Writer, error) {
	if err := w.ctx.Err(); err != nil {
		return nil, nil, err
	}
	if err := spec.Check(); err != nil || spec.DIBBytes == 0 || w.png.handle != 0 {
		return nil, nil, clipboardimage.ErrBounds
	}
	if err := w.png.stage(spec.PNGBytes); err != nil {
		return nil, nil, err
	}
	if err := w.dib.stage(spec.DIBBytes); err != nil {
		w.Close()
		return nil, nil, err
	}
	return &w.png, &w.dib, nil
}

func (w *windowsClipboardOffer) Publish() error {
	if err := w.ctx.Err(); err != nil {
		return err
	}
	if !w.png.complete() || !w.dib.complete() {
		return io.ErrUnexpectedEOF
	}
	if err := errors.Join(w.png.unlock(), w.dib.unlock()); err != nil {
		return err
	}
	format, err := pngClipboardFormat()
	if err != nil {
		return err
	}
	return w.place(format)
}

func (w *windowsClipboardOffer) Close() {
	w.png.close()
	w.dib.close()
}

func (w *windowsClipboardOffer) place(pngFormat uintptr) (err error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	ctx, cancel := context.WithTimeout(w.ctx, clipboardWriteTimeout)
	defer cancel()
	if openErr := openClipboardForImage(ctx, w.owner); openErr != nil {
		return openErr
	}
	defer func() {
		if closed, _, e := procCloseClipboard.Call(); closed&clipboardUintBits == 0 {
			err = errors.Join(err, fmt.Errorf("close the Windows clipboard: %w", e))
		}
	}()
	if err = ctx.Err(); err != nil {
		return err
	}
	if emptied, _, e := procEmptyClipboard.Call(); emptied&clipboardUintBits == 0 {
		return fmt.Errorf("empty the Windows clipboard: %w", e)
	}
	if err := w.png.publish(pngFormat); err != nil {
		return fmt.Errorf("the clipboard was emptied, but Windows did not accept PNG: %w", err)
	}
	if err := w.dib.publish(clipboardFormatDIB); err != nil {
		return fmt.Errorf("PNG was published, but Windows did not accept CF_DIB: %w", err)
	}
	return nil
}

func openClipboardForImage(ctx context.Context, owner uintptr) error {
	if owner == 0 {
		return errors.New("copy the image from a visible window")
	}
	retry := time.NewTimer(clipboardRetryFirst)
	defer retry.Stop()
	for delay := clipboardRetryFirst; ; {
		opened, _, openErr := procOpenClipboard.Call(owner)
		if opened&clipboardUintBits != 0 {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("open the Windows clipboard: %w (%w)", ctx.Err(), openErr)
		case <-retry.C:
		}
		delay = min(2*delay, clipboardRetryLongest)
		retry.Reset(delay)
	}
}

type clipboardGlobal struct {
	handle, address uintptr
	length, written int
}

func (b *clipboardGlobal) stage(n int) error {
	handle, _, err := procClipboardGlobalAlloc.Call(clipboardGlobalMoveable, uintptr(n)) //nolint:gosec // Stage validates both exact positive lengths against clipboardimage.MaxBytes.
	if handle == 0 {
		return fmt.Errorf("allocate the Windows clipboard image: %w", err)
	}
	address, _, err := procClipboardGlobalLock.Call(handle)
	if address == 0 {
		_, _, _ = procClipboardGlobalFree.Call(handle)
		return fmt.Errorf("lock the Windows clipboard image: %w", err)
	}
	*b = clipboardGlobal{handle: handle, address: address, length: n}
	return nil
}

func (b *clipboardGlobal) Write(chunk []byte) (int, error) {
	if b.address == 0 || len(chunk) > b.length-b.written {
		return 0, io.ErrShortWrite
	}
	if len(chunk) > 0 {
		dst := b.address + uintptr(b.written)                                                                              //nolint:gosec // The checked offset and chunk length fit the locked native allocation.
		_, _, _ = procClipboardMoveMemory.Call(dst, uintptr(unsafe.Pointer(unsafe.SliceData(chunk))), uintptr(len(chunk))) //nolint:gosec // RtlMoveMemory reads exactly the live chunk bytes.
		b.written += len(chunk)
	}
	return len(chunk), nil
}

func (b *clipboardGlobal) complete() bool {
	return b.handle != 0 && b.written == b.length
}

func (b *clipboardGlobal) unlock() error {
	if b.address == 0 {
		return nil
	}
	ok, _, err := procClipboardGlobalUnlock.Call(b.handle)
	b.address = 0
	if ok == 0 && !errors.Is(err, windows.ERROR_SUCCESS) {
		return fmt.Errorf("unlock the Windows clipboard image: %w", err)
	}
	return nil
}

func (b *clipboardGlobal) publish(format uintptr) error {
	placed, _, err := procSetClipboardData.Call(format, b.handle)
	if placed == 0 {
		return fmt.Errorf("set clipboard data: %w", err)
	}
	b.handle = 0
	return nil
}

func (b *clipboardGlobal) close() {
	_ = b.unlock()
	if b.handle != 0 {
		_, _, _ = procClipboardGlobalFree.Call(b.handle)
	}
	*b = clipboardGlobal{}
}
