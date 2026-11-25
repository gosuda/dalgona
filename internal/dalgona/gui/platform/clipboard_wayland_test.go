//go:build linux

package platform

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"net"
	"os"
	"slices"
	"sync"
	"syscall"
	"testing"
	"time"
)

const (
	specPasteLimit      = 32 << 20
	fakeSelectionOffer  = 0xff000001
	fakePrimaryOffer    = 0xff000002
	fakeWaylandChunk    = 64 << 10
	fakeWaylandDeadline = 20 * time.Second
)

type fakeWaylandGlobal struct {
	name  uint32
	iface string
}

type fakeWayland struct {
	globals   []fakeWaylandGlobal
	offers    []string
	device    func(device uint32) [][]byte
	source    func(pipe *os.File) error
	mu        sync.Mutex
	bound     []string
	received  []string
	sourceErr []error
	running   sync.WaitGroup
}

type fakeWaylandSession struct {
	fake     *fakeWayland
	conn     *net.UnixConn
	fds      []int
	registry uint32
	manager  uint32
}

type fakeWaylandRequest struct {
	object, opcode uint32
}

func dataControlGlobals(manager string) []fakeWaylandGlobal {
	return []fakeWaylandGlobal{{1, "wl_compositor"}, {2, "wl_seat"}, {3, "wl_data_device_manager"}, {4, manager}}
}

func fakeWaylandEvent(sender, opcode uint32, args ...any) []byte {
	var body []byte
	for _, arg := range args {
		switch v := arg.(type) {
		case uint32:
			body = binary.NativeEndian.AppendUint32(body, v)
		case string:
			if len(v) >= 1<<16 {
				panic("Wayland fixture string exceeds a message")
			}
			body = binary.NativeEndian.AppendUint32(body, uint32(len(v)+1)) //nolint:gosec // the checked string length plus NUL fits in uint32
			body = append(append(body, v...), 0)
			for len(body)%4 != 0 {
				body = append(body, 0)
			}
		}
	}
	if len(body) >= 1<<16-8 {
		panic("Wayland fixture exceeds a message")
	}
	head := binary.NativeEndian.AppendUint32(nil, sender)
	head = binary.NativeEndian.AppendUint32(head, uint32(8+len(body))<<16|opcode) //nolint:gosec // the checked frame length including header fits in 16 bits
	return append(head, body...)
}

func selectionEvents(device uint32, offers []string) [][]byte {
	var events [][]byte
	if offers == nil {
		events = append(events, fakeWaylandEvent(device, 1, uint32(0)))
	} else {
		events = append(events, fakeWaylandEvent(device, 0, uint32(fakeSelectionOffer)))
		for _, mime := range offers {
			events = append(events, fakeWaylandEvent(fakeSelectionOffer, 0, mime))
		}
		events = append(events, fakeWaylandEvent(device, 1, uint32(fakeSelectionOffer)))
	}
	return append(events,
		fakeWaylandEvent(device, 0, uint32(fakePrimaryOffer)),
		fakeWaylandEvent(fakePrimaryOffer, 0, "image/png"),
		fakeWaylandEvent(device, 3, uint32(fakePrimaryOffer)),
	)
}

func writeSource(data []byte) func(*os.File) error {
	return func(pipe *os.File) error {
		_, err := pipe.Write(data)
		return err
	}
}

func endlessSource(pipe *os.File) error {
	chunk := bytes.Repeat([]byte{0xff}, fakeWaylandChunk)
	for {
		if _, err := pipe.Write(chunk); err != nil {
			return err
		}
	}
}

func (f *fakeWayland) clipboard(t *testing.T) *WaylandClipboard {
	t.Helper()
	return &WaylandClipboard{dial: func() (*net.UnixConn, error) {
		fds, err := syscall.Socketpair(syscall.AF_UNIX, syscall.SOCK_STREAM|syscall.SOCK_CLOEXEC, 0)
		if err != nil {
			return nil, err
		}
		client, server := fakeUnixConn(t, fds[0]), fakeUnixConn(t, fds[1])
		f.running.Add(1)
		go func() {
			defer f.running.Done()
			(&fakeWaylandSession{fake: f, conn: server}).serve()
		}()
		return client, nil
	}}
}

func fakeUnixConn(t *testing.T, fd int) *net.UnixConn {
	t.Helper()
	file := os.NewFile(uintptr(fd), "fake-wayland")
	conn, err := net.FileConn(file)
	if closeErr := file.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		t.Fatal(err)
	}
	return conn.(*net.UnixConn)
}

func (s *fakeWaylandSession) serve() {
	defer func() {
		_ = s.conn.Close()
		for _, fd := range s.fds {
			_ = syscall.Close(fd)
		}
	}()
	buf, oob := make([]byte, 4096), make([]byte, syscall.CmsgSpace(4*4))
	var pending []byte
	for {
		n, oobn, _, _, err := s.conn.ReadMsgUnix(buf, oob)
		if err != nil || n == 0 {
			return
		}
		pending = append(pending, buf[:n]...)
		s.fds = append(s.fds, fakeRights(oob[:oobn])...)
		pending = s.dispatch(pending)
	}
}

func (s *fakeWaylandSession) dispatch(pending []byte) []byte {
	for len(pending) >= 8 {
		word := binary.NativeEndian.Uint32(pending[4:])
		size := int(word >> 16)
		if len(pending) < size {
			break
		}
		s.request(fakeWaylandRequest{binary.NativeEndian.Uint32(pending), word & 0xffff}, slices.Clone(pending[8:size]))
		pending = pending[size:]
	}
	return pending
}

func fakeRights(oob []byte) []int {
	messages, err := syscall.ParseSocketControlMessage(oob)
	if err != nil {
		return nil
	}
	var fds []int
	for _, m := range messages {
		rights, err := syscall.ParseUnixRights(&m)
		if err == nil {
			fds = append(fds, rights...)
		}
	}
	return fds
}

func fakeString(body []byte) (string, []byte) {
	n := int(binary.NativeEndian.Uint32(body))
	return string(body[4 : 4+n-1]), body[4+(n+3)&^3:]
}

func (s *fakeWaylandSession) request(r fakeWaylandRequest, body []byte) {
	switch r {
	case fakeWaylandRequest{1, 1}:
		s.registry = binary.NativeEndian.Uint32(body)
		for _, g := range s.fake.globals {
			s.emit(fakeWaylandEvent(s.registry, 0, g.name, g.iface, uint32(1)))
		}
	case fakeWaylandRequest{1, 0}:
		s.emit(fakeWaylandEvent(binary.NativeEndian.Uint32(body), 0, uint32(0)))
	case fakeWaylandRequest{s.registry, 0}:
		s.bind(body)
	case fakeWaylandRequest{s.manager, 1}:
		s.dataDevice(binary.NativeEndian.Uint32(body))
	case fakeWaylandRequest{fakeSelectionOffer, 0}:
		mime, _ := fakeString(body)
		s.receive(mime)
	}
}

func (s *fakeWaylandSession) bind(body []byte) {
	iface, rest := fakeString(body[4:])
	s.fake.mu.Lock()
	s.fake.bound = append(s.fake.bound, iface)
	s.fake.mu.Unlock()
	if iface != "wl_seat" {
		s.manager = binary.NativeEndian.Uint32(rest[4:])
	}
}

func (s *fakeWaylandSession) dataDevice(device uint32) {
	events := selectionEvents(device, s.fake.offers)
	if s.fake.device != nil {
		events = s.fake.device(device)
	}
	for _, e := range events {
		s.emit(e)
	}
}

func (s *fakeWaylandSession) receive(mime string) {
	if len(s.fds) == 0 {
		return
	}
	pipe := os.NewFile(uintptr(s.fds[0]), "fake-source")
	s.fds = s.fds[1:]
	s.fake.mu.Lock()
	s.fake.received = append(s.fake.received, mime)
	s.fake.mu.Unlock()
	s.fake.running.Add(1)
	go func() {
		defer s.fake.running.Done()
		err := s.fake.source(pipe)
		_ = pipe.Close()
		s.fake.mu.Lock()
		s.fake.sourceErr = append(s.fake.sourceErr, err)
		s.fake.mu.Unlock()
	}()
}

func (s *fakeWaylandSession) emit(event []byte) {
	_, _ = s.conn.Write(event)
}

func (f *fakeWayland) receivedMIMEs() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.received)
}

func openFDs(t *testing.T) int {
	t.Helper()
	entries, err := os.ReadDir("/proc/self/fd")
	if err != nil {
		t.Skipf("count open descriptors: %v", err)
	}
	return len(entries)
}

func pasteFromFake(t *testing.T, f *fakeWayland, kind PasteKind) (ClipboardContent, error) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), fakeWaylandDeadline)
	defer cancel()
	content, err := f.clipboard(t).ReadClipboard(ctx, kind)
	f.running.Wait()
	return content, err
}

type boundedPaste struct {
	name   string
	source func(*os.File) error
	want   []byte
	err    error
	broken bool
}

func TestWaylandClipboardBoundsTheTransferBeforeDecoding(t *testing.T) {
	warm := &fakeWayland{globals: dataControlGlobals("ext_data_control_manager_v1"), offers: []string{"text/plain;charset=utf-8"}, source: writeSource([]byte("x"))}
	if _, err := pasteFromFake(t, warm, PasteOrdinary); err != nil {
		t.Fatal(err)
	}
	exact := bytes.Repeat([]byte("a"), specPasteLimit)
	cases := []boundedPaste{
		{"exactly 32 MiB", writeSource(exact), exact, nil, false},
		{"one byte over 32 MiB of invalid UTF-8", writeSource(bytes.Repeat([]byte{0xff}, specPasteLimit+1)), nil, ErrTransferTooLarge, false},
		{"a source that never stops", endlessSource, nil, ErrTransferTooLarge, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, tc.check)
	}
}

func (tc boundedPaste) check(t *testing.T) {
	before := openFDs(t)
	f := &fakeWayland{globals: dataControlGlobals("ext_data_control_manager_v1"), offers: []string{"text/plain;charset=utf-8"}, source: tc.source}
	content, err := pasteFromFake(t, f, PasteOrdinary)
	if !errors.Is(err, tc.err) || content.Text != string(tc.want) {
		t.Fatalf("pasted %d bytes with error %v; want %d bytes and %v", len(content.Text), err, len(tc.want), tc.err)
	}
	if broken := len(f.sourceErr) == 1 && errors.Is(f.sourceErr[0], syscall.EPIPE); broken != tc.broken || len(f.sourceErr) != 1 {
		t.Fatalf("source write errors %v; want one transfer whose stream closed early: %v", f.sourceErr, tc.broken)
	}
	if after := openFDs(t); after != before {
		t.Fatalf("%d open descriptors after the paste, %d before", after, before)
	}
}

func TestWaylandClipboardNegotiatesTheOfferedMIMEType(t *testing.T) {
	png := []byte("\x89PNG\r\n\x1a\n")
	payload := map[string][]byte{"text/plain;charset=UTF-8": []byte("café\n"), "image/png": png, "text/uri-list": []byte("file:///tmp/a%20b.png\r\n")}
	cases := []struct {
		name    string
		offers  []string
		kind    PasteKind
		receive string
		want    ClipboardContent
	}{
		{"text keeps the offered spelling", []string{"TEXT", "text/plain;charset=UTF-8", "image/png"}, PasteOrdinary, "text/plain;charset=UTF-8", ClipboardContent{Text: "café\n"}},
		{"Paste image prefers the image", []string{"text/plain;charset=utf-8", "image/png"}, PasteImage, "image/png", ClipboardContent{Image: png, ImageType: "image/png"}},
		{"Paste prefers files", []string{"UTF8_STRING", "text/uri-list"}, PasteOrdinary, "text/uri-list", ClipboardContent{Files: []string{"/tmp/a b.png"}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := &fakeWayland{globals: dataControlGlobals("zwlr_data_control_manager_v1"), offers: tc.offers, source: writeSource(payload[tc.receive])}
			content, err := pasteFromFake(t, f, tc.kind)
			if err != nil {
				t.Fatal(err)
			}
			if !slices.Equal(f.received, []string{tc.receive}) {
				t.Fatalf("received %q, want %q", f.received, tc.receive)
			}
			if content.Text != tc.want.Text || !bytes.Equal(content.Image, tc.want.Image) || content.ImageType != tc.want.ImageType || !slices.Equal(content.Files, tc.want.Files) {
				t.Fatalf("content %+v, want %+v", content, tc.want)
			}
		})
	}
}

func TestWaylandClipboardPrefersExtDataControl(t *testing.T) {
	globals := []fakeWaylandGlobal{{1, "zwlr_data_control_manager_v1"}, {2, "wl_seat"}, {3, "ext_data_control_manager_v1"}, {4, "wl_seat"}}
	f := &fakeWayland{globals: globals, offers: []string{"UTF8_STRING"}, source: writeSource([]byte("x"))}
	if _, err := pasteFromFake(t, f, PasteOrdinary); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(f.bound, []string{"wl_seat", "ext_data_control_manager_v1"}) {
		t.Fatalf("bound %q, want the first seat and ext_data_control_manager_v1", f.bound)
	}
}

func TestWaylandClipboardRefusesWithoutATransfer(t *testing.T) {
	finished := func(device uint32) [][]byte { return [][]byte{fakeWaylandEvent(device, 2)} }
	truncated := func(device uint32) [][]byte {
		return [][]byte{fakeWaylandEvent(device, 0, uint32(fakeSelectionOffer)), fakeWaylandEvent(fakeSelectionOffer, 0, uint32(64))}
	}
	full := dataControlGlobals("ext_data_control_manager_v1")
	cases := []struct {
		name string
		fake *fakeWayland
		want error
	}{
		{"no data-control global", &fakeWayland{globals: []fakeWaylandGlobal{{1, "wl_seat"}, {2, "wl_data_device_manager"}}}, ErrNoDataControl},
		{"no seat", &fakeWayland{globals: []fakeWaylandGlobal{{1, "ext_data_control_manager_v1"}}}, ErrNoDataControl},
		{"an empty clipboard", &fakeWayland{globals: full}, ErrUnsupportedPaste},
		{"only unsupported formats", &fakeWayland{globals: full, offers: []string{"text/html", "application/x-rtf"}}, ErrUnsupportedPaste},
		{"access ended", &fakeWayland{globals: full, device: finished}, errWaylandAccessEnded},
		{"a malformed string", &fakeWayland{globals: full, device: truncated}, errWaylandProtocol},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := pasteFromFake(t, tc.fake, PasteOrdinary); !errors.Is(err, tc.want) || len(tc.fake.received) != 0 {
				t.Fatalf("paste error %v after receiving %q; want %v and no transfer", err, tc.fake.received, tc.want)
			}
		})
	}
}

func TestWaylandClipboardReportsTheCompositorError(t *testing.T) {
	refused := func(uint32) [][]byte { return [][]byte{fakeWaylandEvent(1, 0, uint32(6), uint32(3), "invalid object")} }
	f := &fakeWayland{globals: dataControlGlobals("ext_data_control_manager_v1"), device: refused}
	_, err := pasteFromFake(t, f, PasteOrdinary)
	if err == nil || errors.Is(err, errWaylandProtocol) || !bytes.Contains([]byte(err.Error()), []byte("invalid object")) {
		t.Fatalf("paste error %v, want the compositor's refusal", err)
	}
}

func TestWaylandClipboardProbeNamesMissingDataControl(t *testing.T) {
	f := &fakeWayland{globals: []fakeWaylandGlobal{{1, "wl_seat"}, {2, "wl_data_device_manager"}}}
	if err := f.clipboard(t).probe(); !errors.Is(err, ErrNoDataControl) {
		t.Fatalf("probe error %v, want ErrNoDataControl", err)
	}
	f.running.Wait()
	f = &fakeWayland{globals: dataControlGlobals("zwlr_data_control_manager_v1")}
	if err := f.clipboard(t).probe(); err != nil {
		t.Fatalf("probe error %v with wlr-data-control offered", err)
	}
	f.running.Wait()
}

func TestWaylandClipboardStopsWhenThePasteEnds(t *testing.T) {
	started, release := make(chan struct{}), make(chan struct{})
	f := &fakeWayland{globals: dataControlGlobals("ext_data_control_manager_v1"), offers: []string{"UTF8_STRING"}, source: func(*os.File) error {
		close(started)
		<-release
		return nil
	}}
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() {
		_, err := f.clipboard(t).ReadClipboard(ctx, PasteOrdinary)
		done <- err
	}()
	<-started
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("paste error %v, want the cancellation", err)
		}
	case <-time.After(fakeWaylandDeadline):
		t.Fatal("the paste kept waiting for a silent source after it was cancelled")
	}
	close(release)
	f.running.Wait()
	if got := f.receivedMIMEs(); !slices.Equal(got, []string{"UTF8_STRING"}) {
		t.Fatalf("received %q, want one UTF8_STRING transfer", got)
	}
}

func TestWaylandRequestFollowsTheWireFormat(t *testing.T) {
	var want []byte
	for _, word := range []uint32{5, 24 << 16, 10} {
		want = binary.NativeEndian.AppendUint32(want, word)
	}
	want = append(want, "image/png\x00\x00\x00"...)
	if got := newWaylandRequest(5, 0).string("image/png").bytes(); !bytes.Equal(got, want) {
		t.Fatalf("receive(\"image/png\") encodes as % x, want % x", got, want)
	}
	want = binary.NativeEndian.AppendUint32(nil, 1)
	want = binary.NativeEndian.AppendUint32(want, 12<<16|1)
	want = binary.NativeEndian.AppendUint32(want, 2)
	if got := newWaylandRequest(1, 1).uint(2).bytes(); !bytes.Equal(got, want) {
		t.Fatalf("get_registry encodes as % x, want % x", got, want)
	}
}

func TestWaylandSocketFollowsTheClientEnvironment(t *testing.T) {
	cases := []struct {
		display, runtime, want string
		err                    error
	}{
		{"wayland-1", "/run/user/1000", "/run/user/1000/wayland-1", nil},
		{"", "/run/user/1000", "/run/user/1000/wayland-0", nil},
		{"/tmp/compositor.sock", "", "/tmp/compositor.sock", nil},
		{"wayland-1", "", "", errWaylandNoRuntimeDir},
	}
	for _, tc := range cases {
		t.Setenv("WAYLAND_DISPLAY", tc.display)
		t.Setenv("XDG_RUNTIME_DIR", tc.runtime)
		if got, err := waylandSocket(); got != tc.want || !errors.Is(err, tc.err) {
			t.Errorf("WAYLAND_DISPLAY=%q XDG_RUNTIME_DIR=%q: %q, %v; want %q, %v", tc.display, tc.runtime, got, err, tc.want, tc.err)
		}
	}
}

func TestWaylandClipboardAcceptsPlainTextAliases(t *testing.T) {
	cases := []struct {
		offer string
		want  string
	}{
		{"text/plain", "text/plain"},
		{"text/plain;charset=utf8", "text/plain;charset=utf8"},
		{"TEXT/PLAIN", "TEXT/PLAIN"},
	}
	for _, tc := range cases {
		t.Run(tc.offer, func(t *testing.T) {
			f := &fakeWayland{globals: dataControlGlobals("ext_data_control_manager_v1"), offers: []string{tc.offer}, source: writeSource([]byte("plain"))}
			content, err := pasteFromFake(t, f, PasteOrdinary)
			if err != nil || content.Text != "plain" {
				t.Fatalf("the %q offer pasted %q with %v, want the text", tc.offer, content.Text, err)
			}
			if !slices.Equal(f.receivedMIMEs(), []string{tc.want}) {
				t.Fatalf("received %q, want the offered %q spelling", f.receivedMIMEs(), tc.want)
			}
		})
	}
}

func TestWaylandClipboardEndsAStalledTransfer(t *testing.T) {
	old := waylandTransferTimeout
	waylandTransferTimeout = 300 * time.Millisecond
	defer func() { waylandTransferTimeout = old }()
	started, release := make(chan struct{}), make(chan struct{})
	f := &fakeWayland{globals: dataControlGlobals("ext_data_control_manager_v1"), offers: []string{"UTF8_STRING"}, source: func(*os.File) error {
		close(started)
		<-release
		return nil
	}}
	done := make(chan error, 1)
	go func() {
		_, err := f.clipboard(t).ReadClipboard(context.Background(), PasteOrdinary)
		done <- err
	}()
	<-started
	select {
	case err := <-done:
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("a stalled transfer ended with %v, want the transfer deadline", err)
		}
	case <-time.After(fakeWaylandDeadline):
		t.Fatal("a stalled transfer outlived its deadline")
	}
	close(release)
	f.running.Wait()
}
