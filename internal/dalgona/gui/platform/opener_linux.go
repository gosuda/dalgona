//go:build linux

package platform

import (
	"context"
	"errors"
	"fmt"
	"os"

	"github.com/godbus/dbus/v5"
)

const portalOpenURI = "org.freedesktop.portal.OpenURI"

var errOpenFailed = errors.New("the system could not open the item; open it from your file manager or browser")

func OpenLink(ctx context.Context, rawURL string) error {
	return OpenURL(ctx, rawURL, linkSchemes...)
}

func OpenURL(ctx context.Context, rawURL string, schemes ...string) error {
	uri, err := checkOpenURL(rawURL, schemes)
	if err != nil {
		return err
	}
	return withSessionBus(func(conn *dbus.Conn) error {
		return openWithPortal(ctx, conn, "OpenURI", uri)
	})
}

func OpenPath(ctx context.Context, path string) error {
	return openPathWithPortal(ctx, path, "OpenFile")
}

func RevealPath(ctx context.Context, path string) error {
	return openPathWithPortal(ctx, path, "OpenDirectory")
}

func openPathWithPortal(ctx context.Context, path, method string) error {
	if err := checkOpenPath(path); err != nil {
		return err
	}
	f, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("open %q for the desktop portal: %w", path, err)
	}
	defer f.Close()
	return withSessionBus(func(conn *dbus.Conn) error {
		return openWithPortal(ctx, conn, method, dbus.UnixFD(f.Fd())) //nolint:gosec // a Linux file descriptor is a nonnegative C int
	})
}

func withSessionBus(use func(conn *dbus.Conn) error) error {
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		return fmt.Errorf("%w: %w", ErrPortalUnavailable, err)
	}
	defer conn.Close()
	return use(conn)
}

func openWithPortal(ctx context.Context, conn *dbus.Conn, method string, target any) error {
	answer, err := askPortal(ctx, conn, portalCall{
		method:  portalOpenURI + "." + method,
		args:    []any{"", target},
		options: map[string]dbus.Variant{},
	})
	if err != nil {
		return err
	}
	switch answer.code {
	case portalResponseSuccess:
		return nil
	case portalResponseCancelled:
		return ErrOpenCancelled
	}
	return errOpenFailed
}
