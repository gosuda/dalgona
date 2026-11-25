//go:build linux

package platform

import (
	"bufio"
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

const portalTestBusConfig = `<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
`

func startPortalTestBus(t *testing.T) string {
	t.Helper()
	daemon, err := exec.LookPath("dbus-daemon")
	if err != nil {
		t.Skip("dbus-daemon is not installed; the portal tests need a private session bus")
	}
	config := filepath.Join(t.TempDir(), "bus.conf")
	if err = os.WriteFile(config, []byte(portalTestBusConfig), 0o600); err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(daemon, "--config-file="+config, "--nofork", "--print-address")
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
	})
	address, err := bufio.NewReader(out).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	return strings.TrimSpace(address)
}

func connectPortalTestBus(t *testing.T, address string) *dbus.Conn {
	t.Helper()
	conn, err := dbus.Connect(address)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

type portalCallRecord struct {
	method  string
	parent  string
	title   string
	options map[string]dbus.Variant
}

type fakeFileChooser struct {
	conn    *dbus.Conn
	code    uint32
	uris    []string
	silent  bool
	mu      sync.Mutex
	calls   []portalCallRecord
	closed  chan dbus.ObjectPath
	request dbus.ObjectPath
}

type fakePortalRequest struct {
	path   dbus.ObjectPath
	closed chan dbus.ObjectPath
}

func (r fakePortalRequest) Close() *dbus.Error {
	r.closed <- r.path
	return nil
}

func (f *fakeFileChooser) OpenFile(sender dbus.Sender, parent, title string, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	return f.answer("OpenFile", sender, portalCallRecord{parent: parent, title: title, options: options})
}

func (f *fakeFileChooser) SaveFile(sender dbus.Sender, parent, title string, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	return f.answer("SaveFile", sender, portalCallRecord{parent: parent, title: title, options: options})
}

func (f *fakeFileChooser) answer(method string, sender dbus.Sender, call portalCallRecord) (dbus.ObjectPath, *dbus.Error) {
	call.method = method
	var token string
	_ = call.options["handle_token"].Store(&token)
	path := dbus.ObjectPath(portalObject + "/request/" + strings.ReplaceAll(strings.TrimPrefix(string(sender), ":"), ".", "_") + "/" + token)
	f.mu.Lock()
	f.calls = append(f.calls, call)
	f.request = path
	f.mu.Unlock()
	_ = f.conn.Export(fakePortalRequest{path: path, closed: f.closed}, path, portalRequest)
	if !f.silent {
		go func() {
			_ = f.conn.Emit(path, portalRequest+".Response", f.code, map[string]dbus.Variant{"uris": dbus.MakeVariant(f.uris)})
		}()
	}
	return path, nil
}

func (f *fakeFileChooser) lastCall(t *testing.T) portalCallRecord {
	t.Helper()
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.calls) != 1 {
		t.Fatalf("the portal received %d calls, want 1", len(f.calls))
	}
	return f.calls[0]
}

func servePortal(t *testing.T, address string, chooser *fakeFileChooser) {
	t.Helper()
	chooser.conn = connectPortalTestBus(t, address)
	chooser.closed = make(chan dbus.ObjectPath, 1)
	if err := chooser.conn.Export(chooser, portalObject, portalFileChooser); err != nil {
		t.Fatal(err)
	}
	if _, err := chooser.conn.RequestName(portalService, dbus.NameFlagDoNotQueue); err != nil {
		t.Fatal(err)
	}
}

func portalDialog(t *testing.T, chooser *fakeFileChooser, req DialogRequest) (DialogResult, error) {
	t.Helper()
	address := startPortalTestBus(t)
	servePortal(t, address, chooser)
	client := connectPortalTestBus(t, address)
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	return PortalDialog(ctx, client, "x11:4a00007", req)
}

func optionValue[T any](t *testing.T, options map[string]dbus.Variant, key string) T {
	t.Helper()
	var value T
	v, ok := options[key]
	if !ok {
		t.Fatalf("option %q is missing from %v", key, options)
	}
	if err := v.Store(&value); err != nil {
		t.Fatalf("option %q: %v", key, err)
	}
	return value
}

func TestPortalOpenFilesReturnsLocalPathsInOrder(t *testing.T) {
	chooser := &fakeFileChooser{uris: []string{"file:///home/u/z%20last.png", "file:///home/u/%ED%95%9C.png"}}
	got, err := portalDialog(t, chooser, DialogRequest{
		Kind: DialogOpenFiles, Title: "Attach files",
		Filters: []FileFilter{{Name: "Images", Extensions: []string{"png", "jpg"}}},
	})
	if err != nil || got.Cancelled || !slices.Equal(got.Files, []string{"/home/u/z last.png", "/home/u/한.png"}) {
		t.Fatalf("result %+v, error %v", got, err)
	}
	call := chooser.lastCall(t)
	if call.method != "OpenFile" || call.parent != "x11:4a00007" || call.title != "Attach files" {
		t.Fatalf("portal call %+v", call)
	}
	checkOpenFileOptions(t, call.options)
}

func checkOpenFileOptions(t *testing.T, options map[string]dbus.Variant) {
	t.Helper()
	if !optionValue[bool](t, options, "multiple") || !optionValue[bool](t, options, "modal") {
		t.Fatal("open files must ask for a modal multiple selection")
	}
	filters := optionValue[[]portalFilter](t, options, "filters")
	want := []portalFilter{{Name: "Images", Patterns: []portalPattern{{0, "*.[pP][nN][gG]"}, {0, "*.[jJ][pP][gG]"}}}}
	if len(filters) != 1 || filters[0].Name != want[0].Name || !slices.Equal(filters[0].Patterns, want[0].Patterns) {
		t.Fatalf("filters %+v, want %+v", filters, want)
	}
}

func TestPortalChooseFolderAsksForADirectory(t *testing.T) {
	chooser := &fakeFileChooser{uris: []string{"file:///home/u/work%20space"}}
	got, err := portalDialog(t, chooser, DialogRequest{Kind: DialogChooseFolder, Title: "Choose workspace folder"})
	if err != nil || !slices.Equal(got.Files, []string{"/home/u/work space"}) {
		t.Fatalf("result %+v, error %v", got, err)
	}
	call := chooser.lastCall(t)
	if call.method != "OpenFile" || !optionValue[bool](t, call.options, "directory") {
		t.Fatalf("folder choice call %+v", call)
	}
	if _, ok := call.options["multiple"]; ok {
		t.Fatal("a folder choice asked for multiple selection")
	}
}

func TestPortalSaveFileSuggestsTheName(t *testing.T) {
	chooser := &fakeFileChooser{uris: []string{"file:///home/u/notes.md"}}
	got, err := portalDialog(t, chooser, DialogRequest{Kind: DialogSaveFile, Title: "Export document", SuggestedName: "notes.md"})
	if err != nil || !slices.Equal(got.Files, []string{"/home/u/notes.md"}) {
		t.Fatalf("result %+v, error %v", got, err)
	}
	call := chooser.lastCall(t)
	if call.method != "SaveFile" || optionValue[string](t, call.options, "current_name") != "notes.md" {
		t.Fatalf("save call %+v", call)
	}
}

func TestPortalCancellationIsADistinctResult(t *testing.T) {
	got, err := portalDialog(t, &fakeFileChooser{code: 1}, DialogRequest{Kind: DialogOpenFiles, Title: "Attach files"})
	if err != nil || !got.Cancelled || got.Files != nil {
		t.Fatalf("cancelled dialog: result %+v, error %v", got, err)
	}
	if _, err := portalDialog(t, &fakeFileChooser{code: 2}, DialogRequest{Kind: DialogOpenFiles, Title: "Attach files"}); !errors.Is(err, errPortalEnded) {
		t.Fatalf("dialog ended otherwise: %v", err)
	}
}

func TestPortalRefusesRemoteSelections(t *testing.T) {
	chooser := &fakeFileChooser{uris: []string{"file:///home/u/a", "sftp://host/b"}}
	if got, err := portalDialog(t, chooser, DialogRequest{Kind: DialogOpenFiles, Title: "Attach files"}); !errors.Is(err, ErrNotLocalFile) {
		t.Fatalf("remote selection: result %+v, error %v", got, err)
	}
}

func TestPortalClosesTheRequestWhenTheWindowGenerationEnds(t *testing.T) {
	address := startPortalTestBus(t)
	chooser := &fakeFileChooser{silent: true}
	servePortal(t, address, chooser)
	client := connectPortalTestBus(t, address)
	var lifetime Lifetime
	_, pending := lifetime.Begin(t.Context())
	done := make(chan error, 1)
	go func() {
		_, err := PortalDialog(pending, client, "", DialogRequest{Kind: DialogOpenFiles, Title: "Attach files"})
		done <- err
	}()
	deadline := time.After(10 * time.Second)
	for chooser.lastRequest() == "" {
		select {
		case <-deadline:
			t.Fatal("the portal never received the request")
		case <-time.After(10 * time.Millisecond):
		}
	}
	lifetime.End()
	if err := <-done; !errors.Is(err, context.Canceled) {
		t.Fatalf("dialog after window close: %v", err)
	}
	select {
	case path := <-chooser.closed:
		if path != chooser.lastRequest() {
			t.Fatalf("closed %s, want %s", path, chooser.lastRequest())
		}
	case <-deadline:
		t.Fatal("the portal request was never closed")
	}
}

func (f *fakeFileChooser) lastRequest() dbus.ObjectPath {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.request
}

func TestPortalUnavailableKeepsTheRequestForGuidance(t *testing.T) {
	address := startPortalTestBus(t)
	client := connectPortalTestBus(t, address)
	_, err := PortalDialog(t.Context(), client, "", DialogRequest{Kind: DialogOpenFiles, Title: "Attach files"})
	if !errors.Is(err, ErrPortalUnavailable) {
		t.Fatalf("no portal service: %v, want ErrPortalUnavailable", err)
	}
	if _, err := PortalDialog(t.Context(), client, "", DialogRequest{Kind: DialogChooseFolder, Title: "x", Filters: []FileFilter{{Name: "a", Extensions: []string{"b"}}}}); err == nil || errors.Is(err, ErrPortalUnavailable) {
		t.Fatalf("an invalid request reached the bus: %v", err)
	}
}

func TestPortalDisconnectEndsThePendingDialog(t *testing.T) {
	responses := make(chan *dbus.Signal)
	close(responses)
	if _, err := awaitPortalResponse(t.Context(), nil, "/org/freedesktop/portal/desktop/request/x/y", responses); !errors.Is(err, errPortalEnded) {
		t.Fatalf("a disconnected bus ended the dialog with %v, want the portal-ended refusal", err)
	}
}

func TestPortalSuccessWithoutURIsEndsTheDialog(t *testing.T) {
	if _, err := dialogResult(portalAnswer{code: portalResponseSuccess}); !errors.Is(err, errPortalEnded) {
		t.Fatalf("a successful reply without a URI list ended with %v, want the portal-ended refusal", err)
	}
}
