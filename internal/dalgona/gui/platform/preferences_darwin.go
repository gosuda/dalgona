//go:build darwin

package platform

// #cgo CFLAGS: -fobjc-arc -x objective-c
// #cgo LDFLAGS: -framework AppKit
// #include "preferences_darwin.h"
import "C"

import (
	"context"
	"fmt"
	"os"
	"syscall"
	"unsafe"
)

func ReducedMotion(ctx context.Context) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	return bool(C.dalgona_preferences_reduce_motion()), nil
}

func HighContrast(ctx context.Context) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	return bool(C.dalgona_preferences_increase_contrast()), nil
}

func PrefersDark(ctx context.Context) (bool, error) {
	if err := ctx.Err(); err != nil {
		return false, err
	}
	return bool(C.dalgona_preferences_dark()), nil
}

func WatchAppearance(ctx context.Context, changed func()) error {
	read, write, err := os.Pipe()
	if err != nil {
		return fmt.Errorf("watch the desktop appearance: create the event pipe: %w", err)
	}
	defer read.Close()
	defer write.Close()
	var token unsafe.Pointer
	if code := C.dalgona_appearance_watch(C.uintptr_t(write.Fd()), &token); code != 0 {
		return fmt.Errorf("watch the desktop appearance: configure the event pipe: %w", syscall.Errno(code)) //nolint:gosec // the native watcher returns a positive errno value
	}
	defer C.dalgona_appearance_unwatch(token)
	cancelRead := context.AfterFunc(ctx, func() { _ = read.Close() })
	defer cancelRead()
	return followAppearancePipe(ctx, read, changed)
}

func followAppearancePipe(ctx context.Context, read *os.File, changed func()) error {
	var batch [32]byte
	for ctx.Err() == nil {
		n, err := read.Read(batch[:])
		if ctx.Err() != nil {
			break
		}
		if err != nil {
			return fmt.Errorf("watch the desktop appearance: read the event pipe: %w", err)
		}
		if n > 0 {
			changed()
		}
	}
	return nil
}
