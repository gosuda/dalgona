//go:build linux

package platform

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

type fakeOpenURI struct {
	conn   *dbus.Conn
	code   uint32
	mu     sync.Mutex
	method string
	uri    string
	opened string
}

func (f *fakeOpenURI) respond(sender dbus.Sender, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	var token string
	_ = options["handle_token"].Store(&token)
	path := dbus.ObjectPath(portalObject + "/request/" + strings.ReplaceAll(strings.TrimPrefix(string(sender), ":"), ".", "_") + "/" + token)
	go func() {
		_ = f.conn.Emit(path, portalRequest+".Response", f.code, map[string]dbus.Variant{})
	}()
	return path, nil
}

func (f *fakeOpenURI) OpenURI(sender dbus.Sender, _ string, uri string, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	f.mu.Lock()
	f.method, f.uri = "OpenURI", uri
	f.mu.Unlock()
	return f.respond(sender, options)
}

func (f *fakeOpenURI) OpenFile(sender dbus.Sender, _ string, fd dbus.UnixFD, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	return f.openFD(sender, "OpenFile", fd, options)
}

func (f *fakeOpenURI) OpenDirectory(sender dbus.Sender, _ string, fd dbus.UnixFD, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	return f.openFD(sender, "OpenDirectory", fd, options)
}

func (f *fakeOpenURI) openFD(sender dbus.Sender, method string, fd dbus.UnixFD, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	target, _ := os.Readlink("/proc/self/fd/" + strconv.Itoa(int(fd)))
	f.mu.Lock()
	f.method, f.opened = method, target
	f.mu.Unlock()
	return f.respond(sender, options)
}

func serveOpenURI(t *testing.T, code uint32) *fakeOpenURI {
	t.Helper()
	address := startPortalTestBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	f := &fakeOpenURI{conn: connectPortalTestBus(t, address), code: code}
	if err := f.conn.Export(f, portalObject, portalOpenURI); err != nil {
		t.Fatal(err)
	}
	if _, err := f.conn.RequestName(portalService, dbus.NameFlagDoNotQueue); err != nil {
		t.Fatal(err)
	}
	return f
}

func (f *fakeOpenURI) seen() (method, uri, opened string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.method, f.uri, f.opened
}

func openerContext(t *testing.T) context.Context {
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	t.Cleanup(cancel)
	return ctx
}

func TestOpenURLHandsTheExactURLToThePortal(t *testing.T) {
	portal := serveOpenURI(t, 0)
	const link = "https://github.com/login/device?code=AB-12&x=%20"
	if err := OpenURL(openerContext(t), link, "https"); err != nil {
		t.Fatal(err)
	}
	if method, uri, _ := portal.seen(); method != "OpenURI" || uri != link {
		t.Fatalf("portal received %s %q, want OpenURI %q", method, uri, link)
	}
	if err := OpenURL(openerContext(t), "file:///etc/passwd", "https", "file"); !errors.Is(err, ErrOpenRefused) {
		t.Fatalf("file URL: %v", err)
	}
}

func TestOpenPathPassesTheFileItselfToThePortal(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "My Notes 한글 #1")
	if err := os.Mkdir(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	file := filepath.Join(dir, "a b%20.md")
	if err := os.WriteFile(file, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	portal := serveOpenURI(t, 0)
	if err := OpenPath(openerContext(t), file); err != nil {
		t.Fatal(err)
	}
	if method, _, opened := portal.seen(); method != "OpenFile" || opened != file {
		t.Fatalf("portal opened %s %q, want OpenFile %q", method, opened, file)
	}
	if err := RevealPath(openerContext(t), file); err != nil {
		t.Fatal(err)
	}
	if method, _, opened := portal.seen(); method != "OpenDirectory" || opened != file {
		t.Fatalf("portal revealed %s %q, want OpenDirectory %q", method, opened, file)
	}
}

func TestOpenReportsACancelledChoice(t *testing.T) {
	serveOpenURI(t, 1)
	if err := OpenURL(openerContext(t), "https://example.com", "https"); !errors.Is(err, ErrOpenCancelled) {
		t.Fatalf("cancelled app choice: %v", err)
	}
}
