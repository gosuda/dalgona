//go:build gui && linux

package desktop

import (
	"bufio"
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"gioui.org/app"
	"github.com/godbus/dbus/v5"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

const desktopTestBusConfig = `<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
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

func startDesktopTestBus(t *testing.T) string {
	t.Helper()
	daemon, err := exec.LookPath("dbus-daemon")
	if err != nil {
		t.Skip("dbus-daemon is not installed; the desktop window test needs a private session bus")
	}
	config := filepath.Join(t.TempDir(), "bus.conf")
	if err = os.WriteFile(config, []byte(desktopTestBusConfig), 0o600); err != nil {
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

type heldFileChooser struct {
	conn    *dbus.Conn
	mu      sync.Mutex
	parents []string
	release chan struct{}
	closed  chan dbus.ObjectPath
}

type heldRequest struct {
	path   dbus.ObjectPath
	closed chan dbus.ObjectPath
}

func (r heldRequest) Close() *dbus.Error {
	r.closed <- r.path
	return nil
}

func (f *heldFileChooser) OpenFile(sender dbus.Sender, parent, _ string, options map[string]dbus.Variant) (dbus.ObjectPath, *dbus.Error) {
	var token string
	_ = options["handle_token"].Store(&token)
	path := dbus.ObjectPath("/org/freedesktop/portal/desktop/request/" + strings.ReplaceAll(strings.TrimPrefix(string(sender), ":"), ".", "_") + "/" + token)
	f.mu.Lock()
	f.parents = append(f.parents, parent)
	f.mu.Unlock()
	_ = f.conn.Export(heldRequest{path: path, closed: f.closed}, path, "org.freedesktop.portal.Request")
	go func() {
		<-f.release
		_ = f.conn.Emit(path, "org.freedesktop.portal.Request.Response", uint32(0),
			map[string]dbus.Variant{"uris": dbus.MakeVariant([]string{"file:///home/u/a%20b.txt"})})
	}()
	return path, nil
}

func serveHeldChooser(t *testing.T, address string) *heldFileChooser {
	t.Helper()
	conn, err := dbus.Connect(address)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	f := &heldFileChooser{conn: conn, release: make(chan struct{}), closed: make(chan dbus.ObjectPath, 1)}
	if err := conn.Export(f, "/org/freedesktop/portal/desktop", "org.freedesktop.portal.FileChooser"); err != nil {
		t.Fatal(err)
	}
	if _, err := conn.RequestName("org.freedesktop.portal.Desktop", dbus.NameFlagDoNotQueue); err != nil {
		t.Fatal(err)
	}
	return f
}

func x11View() app.X11ViewEvent {
	return app.X11ViewEvent{Window: 0x4a00007}
}

func TestWindowDialogFollowsTheViewGeneration(t *testing.T) {
	address := startDesktopTestBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	t.Setenv("AT_SPI_BUS_ADDRESS", address)
	t.Setenv("DISPLAY", "")
	chooser := serveHeldChooser(t, address)
	w := New(new(app.Window), platform.NewAccessibilityTree(4, nil))
	attach := []platform.DialogRequest{{Kind: platform.DialogOpenFiles, Title: "Attach files"}}
	if err := w.ShowDialog(attach[0]); !errors.Is(err, ErrNoView) {
		t.Fatalf("dialog before a view: %v, want ErrNoView", err)
	}
	_ = w.HandleEvent(t.Context(), x11View())
	if err := w.ShowDialog(attach[0]); err != nil {
		t.Fatal(err)
	}
	if err := w.ShowDialog(attach[0]); !errors.Is(err, ErrDialogOpen) {
		t.Fatalf("second dialog: %v, want ErrDialogOpen", err)
	}
	close(chooser.release)
	select {
	case got := <-w.DialogOutcomes():
		if got.Err != nil || !slices.Equal(got.Result.Files, []string{"/home/u/a b.txt"}) {
			t.Fatalf("outcome %+v", got)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the dialog outcome never arrived")
	}
	chooser.mu.Lock()
	parents := slices.Clone(chooser.parents)
	chooser.mu.Unlock()
	if !slices.Equal(parents, []string{"x11:4a00007"}) {
		t.Fatalf("portal parents %q, want the X11 window", parents)
	}
}

func TestWindowCloseCancelsItsDialog(t *testing.T) {
	address := startDesktopTestBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	t.Setenv("AT_SPI_BUS_ADDRESS", address)
	t.Setenv("DISPLAY", "")
	chooser := serveHeldChooser(t, address)
	w := New(new(app.Window), platform.NewAccessibilityTree(4, nil))
	_ = w.HandleEvent(t.Context(), x11View())
	if err := w.ShowDialog(platform.DialogRequest{Kind: platform.DialogChooseFolder, Title: "Choose workspace folder"}); err != nil {
		t.Fatal(err)
	}
	deadline := time.After(10 * time.Second)
	waitPortalRequest(t, chooser, deadline)
	_ = w.HandleEvent(t.Context(), app.X11ViewEvent{})
	expectPortalClosed(t, chooser, deadline)
	expectNoDialogOutcome(t, w, chooser)
}

func waitPortalRequest(t *testing.T, chooser *heldFileChooser, deadline <-chan time.Time) {
	t.Helper()
	for {
		chooser.mu.Lock()
		asked := len(chooser.parents)
		chooser.mu.Unlock()
		if asked == 1 {
			return
		}
		select {
		case <-deadline:
			t.Fatal("the portal never received the dialog")
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func expectPortalClosed(t *testing.T, chooser *heldFileChooser, deadline <-chan time.Time) {
	t.Helper()
	select {
	case <-chooser.closed:
	case <-deadline:
		t.Fatal("closing the window did not close the portal request")
	}
}

func expectNoDialogOutcome(t *testing.T, w *Window, chooser *heldFileChooser) {
	t.Helper()
	close(chooser.release)
	select {
	case got := <-w.DialogOutcomes():
		t.Fatalf("a dialog outcome reached a closed window: %+v", got)
	case <-time.After(200 * time.Millisecond):
	}
}

func TestWindowPublishesDialogHandlesAtomically(t *testing.T) {
	address := startDesktopTestBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", address)
	t.Setenv("AT_SPI_BUS_ADDRESS", address)
	t.Setenv("DISPLAY", "")
	w := New(new(app.Window), platform.NewAccessibilityTree(4, nil))
	_ = w.HandleEvent(t.Context(), x11View())
	var racers sync.WaitGroup
	for i := 0; i < 16; i++ {
		racers.Add(2)
		go func() { defer racers.Done(); _ = w.HandleEvent(t.Context(), x11View()) }()
		go func() {
			defer racers.Done()
			_ = w.ShowDialog(platform.DialogRequest{Kind: platform.DialogOpenFiles, Title: "Attach files"})
		}()
	}
	racers.Wait()
	_ = w.HandleEvent(t.Context(), app.X11ViewEvent{})
	for {
		select {
		case <-w.DialogOutcomes():
		case <-time.After(500 * time.Millisecond):
			return
		}
	}
}

type stalledX11 struct {
	path     string
	display  string
	listener *net.UnixListener
	release  chan struct{}
	atoms    atomic.Uint32
	served   sync.WaitGroup
}

func startStalledX11(t *testing.T) *stalledX11 {
	t.Helper()
	if err := os.MkdirAll("/tmp/.X11-unix", 0o777); err != nil {
		t.Skipf("the X socket directory is unavailable: %v", err)
	}
	for number := 400; number < 440; number++ {
		socket := fmt.Sprintf("/tmp/.X11-unix/X%d", number)
		if _, err := os.Stat(socket); err == nil {
			continue
		}
		if _, err := os.Stat(fmt.Sprintf("/tmp/.X%d-lock", number)); err == nil {
			continue
		}
		listener, err := net.ListenUnix("unix", &net.UnixAddr{Name: socket, Net: "unix"})
		if err != nil {
			continue
		}
		server := &stalledX11{path: socket, display: fmt.Sprintf(":%d", number), listener: listener, release: make(chan struct{})}
		server.atoms.Store(0x200)
		t.Cleanup(func() {
			_ = listener.Close()
			server.served.Wait()
			_ = os.Remove(socket)
		})
		go server.serve()
		return server
	}
	t.Fatal("no free X display number for the stalled server")
	return nil
}

func (s *stalledX11) serve() {
	for {
		conn, err := s.listener.AcceptUnix()
		if err != nil {
			return
		}
		s.served.Add(1)
		go func() {
			defer s.served.Done()
			defer conn.Close()
			s.handshake(conn)
		}()
	}
}

func (s *stalledX11) handshake(conn *net.UnixConn) {
	if !s.greet(conn) {
		return
	}
	var sequence uint16
	for {
		opcode, ok := readX11Frame(conn)
		if !ok {
			return
		}
		sequence++
		if !s.answer(conn, opcode, sequence) {
			return
		}
	}
}

func (s *stalledX11) greet(conn *net.UnixConn) bool {
	head := make([]byte, 12)
	if _, err := io.ReadFull(conn, head); err != nil {
		return false
	}
	extra := (int(binary.LittleEndian.Uint16(head[6:]))+3)&^3 + (int(binary.LittleEndian.Uint16(head[8:]))+3)&^3
	if _, err := io.CopyN(io.Discard, conn, int64(extra)); err != nil {
		return false
	}
	_, err := conn.Write(stalledX11Setup())
	return err == nil
}

func readX11Frame(conn *net.UnixConn) (byte, bool) {
	frame := make([]byte, 4)
	if _, err := io.ReadFull(conn, frame); err != nil {
		return 0, false
	}
	length := int(binary.LittleEndian.Uint16(frame[2:]))
	if length < 1 {
		return 0, false
	}
	if _, err := io.CopyN(io.Discard, conn, int64(4*(length-1))); err != nil {
		return 0, false
	}
	return frame[0], true
}

func (s *stalledX11) answer(conn *net.UnixConn, opcode byte, sequence uint16) bool {
	reply := make([]byte, 32)
	reply[0] = 1
	binary.LittleEndian.PutUint16(reply[2:], sequence)
	switch opcode {
	case 16:
		binary.LittleEndian.PutUint32(reply[8:], s.atoms.Add(1))
	case 40:
		<-s.release
		reply[1] = 1
		binary.LittleEndian.PutUint16(reply[12:], 11)
		binary.LittleEndian.PutUint16(reply[14:], 22)
	default:
		return false
	}
	_, err := conn.Write(reply)
	return err == nil
}

func stalledX11Setup() []byte {
	body := make([]byte, 72)
	binary.LittleEndian.PutUint32(body[4:], 0x04000000)
	binary.LittleEndian.PutUint32(body[8:], 0x001FFFFF)
	binary.LittleEndian.PutUint16(body[18:], 4096)
	body[20] = 1
	return append([]byte{1, 0, 11, 0, 0, 0, 18, 0}, body...)
}

func TestWindowOriginServesCachedGeometry(t *testing.T) {
	server := startStalledX11(t)
	t.Setenv("DISPLAY", server.display)
	native := newNative(nil, platform.NewAccessibilityTree(4, nil))
	t.Cleanup(func() {
		select {
		case <-server.release:
		default:
			close(server.release)
		}
		if clipboard, ok := native.reader.(*platform.X11Clipboard); ok {
			_ = clipboard.Close()
		}
	})
	var lifetime platform.Lifetime
	generation, _ := lifetime.Begin(t.Context())
	if err := native.openClipboard(&lifetime, generation, x11View()); err != nil {
		t.Fatal(err)
	}
	origin := native.windowOrigin(0x25, generation)
	expectStalledOrigin(t, origin)
	close(server.release)
	waitCachedOrigin(t, origin)
	hammerCachedOrigin(t, origin)
}

func expectStalledOrigin(t *testing.T, origin func() image.Point) {
	t.Helper()
	done := make(chan image.Point, 1)
	go func() { done <- origin() }()
	select {
	case got := <-done:
		if got != (image.Point{}) {
			t.Fatalf("a stalled origin query returned %v, want the cached zero point", got)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("a stalled X server blocked the screen-reader geometry query")
	}
}

func waitCachedOrigin(t *testing.T, origin func() image.Point) {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		if got := origin(); got == image.Pt(11, 22) {
			return
		}
		select {
		case <-deadline:
			t.Fatal("the origin cache never received the asynchronous refresh")
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func hammerCachedOrigin(t *testing.T, origin func() image.Point) {
	t.Helper()
	var burst sync.WaitGroup
	for range 16 {
		burst.Add(1)
		go func() {
			defer burst.Done()
			if got := origin(); got != image.Pt(11, 22) {
				t.Errorf("a cached origin query returned %v, want (11,22)", got)
			}
		}()
	}
	burst.Wait()
}
