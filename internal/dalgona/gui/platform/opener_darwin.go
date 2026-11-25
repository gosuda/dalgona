//go:build darwin

package platform

// #cgo CFLAGS: -fobjc-arc -x objective-c
// #cgo LDFLAGS: -framework AppKit
// #include "opener_darwin.h"
import "C"

import (
	"context"
	"fmt"
	"os"
)

const (
	appKitOpened = iota
	appKitOpenRefused
	appKitOpenUnreadable
)

func OpenLink(ctx context.Context, rawURL string) error {
	return OpenURL(ctx, rawURL, linkSchemes...)
}

func OpenURL(ctx context.Context, rawURL string, schemes ...string) error {
	uri, err := checkOpenURL(rawURL, schemes)
	if err != nil {
		return err
	}
	return openWithWorkspace(ctx, uri, func(target *C.uint8_t, length C.size_t) C.int { return C.dalgona_open_url(target, length) })
}

func OpenPath(ctx context.Context, path string) error {
	if err := checkOpenPath(path); err != nil {
		return err
	}
	return openWithWorkspace(ctx, path, func(target *C.uint8_t, length C.size_t) C.int { return C.dalgona_open_path(target, length) })
}

func RevealPath(ctx context.Context, path string) error {
	if err := checkOpenPath(path); err != nil {
		return err
	}
	if _, err := os.Lstat(path); err != nil {
		return fmt.Errorf("reveal %q in Finder: %w", path, err)
	}
	return openWithWorkspace(ctx, path, func(target *C.uint8_t, length C.size_t) C.int { return C.dalgona_reveal_path(target, length) })
}

func openWithWorkspace(ctx context.Context, target string, open func(*C.uint8_t, C.size_t) C.int) error {
	status, err := awaitNative(ctx, func() C.int {
		bytes, length := nativeBytes(target)
		return open(bytes, length)
	})
	if err != nil {
		return err
	}
	switch status {
	case appKitOpened:
		return nil
	case appKitOpenUnreadable:
		return fmt.Errorf("macOS cannot read %q as a link or a UTF-8 path; copy it instead", target)
	case appKitOpenRefused:
		return fmt.Errorf("no application opened %q; install an application for this kind of item, or copy it instead", target)
	}
	return fmt.Errorf("macOS answered the request to open %q with unknown status %d; try again", target, int(status))
}
