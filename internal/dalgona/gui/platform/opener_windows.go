//go:build windows

package platform

import (
	"context"
	"fmt"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	shell32                       = windows.NewLazySystemDLL("shell32.dll")
	procILCreateFromPathW         = shell32.NewProc("ILCreateFromPathW")
	procILFree                    = shell32.NewProc("ILFree")
	procSHOpenFolderAndSelectItem = shell32.NewProc("SHOpenFolderAndSelectItems")
)

func OpenLink(ctx context.Context, rawURL string) error {
	return OpenURL(ctx, rawURL, linkSchemes...)
}

func OpenURL(ctx context.Context, rawURL string, schemes ...string) error {
	uri, err := checkOpenURL(rawURL, schemes)
	if err != nil {
		return err
	}
	return shellOpen(ctx, uri)
}

func OpenPath(ctx context.Context, path string) error {
	if err := checkOpenPath(path); err != nil {
		return err
	}
	return shellOpen(ctx, path)
}

func RevealPath(ctx context.Context, path string) error {
	if err := checkOpenPath(path); err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	wide, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return fmt.Errorf("reveal %q: %w", path, err)
	}
	item, _, _ := procILCreateFromPathW.Call(uintptr(unsafe.Pointer(wide))) //nolint:gosec // ILCreateFromPathW reads the live, NUL-terminated UTF-16 path
	if item == 0 {
		return fmt.Errorf("reveal %q: the shell does not know this path; check that it exists", path)
	}
	defer func() { _, _, _ = procILFree.Call(item) }()
	if hr, _, _ := procSHOpenFolderAndSelectItem.Call(item, 0, 0, 0); hr&0x80000000 != 0 {
		return fmt.Errorf("reveal %q in Explorer: %w", path, windows.Errno(hr&0xffffffff))
	}
	return nil
}

func shellOpen(ctx context.Context, target string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	verb, err := windows.UTF16PtrFromString("open")
	if err != nil {
		return err
	}
	file, err := windows.UTF16PtrFromString(target)
	if err != nil {
		return fmt.Errorf("open %q: %w", target, err)
	}
	if err := windows.ShellExecute(0, verb, file, nil, nil, windows.SW_SHOWNORMAL); err != nil {
		return fmt.Errorf("open %q: %w", target, err)
	}
	return nil
}
