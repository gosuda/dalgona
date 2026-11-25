//go:build linux || freebsd

package platform

import (
	"bytes"
	"encoding/binary"
	"errors"
	"image"
	"net"
	"os"
	"slices"
	"sync"
	"syscall"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

type waylandWriteCapture struct {
	mu       sync.Mutex
	sequence []string
	offers   []string
	source   uint32
	selected uint32
	received []byte
}

func (c *waylandWriteCapture) note(step string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.sequence = append(c.sequence, step)
}

type waylandWriteServer struct {
	t        *testing.T
	conn     *net.UnixConn
	pending  []byte
	registry uint32
	source   uint32
	synced   bool
	capture  *waylandWriteCapture
	png      []byte
}

func waylandWritePair(t *testing.T) (*net.UnixConn, *net.UnixConn) {
	t.Helper()
	fds, err := syscall.Socketpair(syscall.AF_UNIX, syscall.SOCK_STREAM|syscall.SOCK_CLOEXEC, 0)
	if err != nil {
		t.Fatal(err)
	}
	client, server := fakeUnixConn(t, fds[0]), fakeUnixConn(t, fds[1])
	t.Cleanup(func() {
		_ = server.Close()
	})
	return client, server
}

func (s *waylandWriteServer) run(done chan<- struct{}) {
	defer close(done)
	buf := make([]byte, 4096)
	for {
		n, err := s.conn.Read(buf)
		if err != nil || n == 0 {
			return
		}
		s.pending = append(s.pending, buf[:n]...)
		s.drain()
		if s.synced && s.source != 0 {
			emitWaylandSend(s.t, s.conn, s.source, s.png, s.capture)
			return
		}
	}
}

func (s *waylandWriteServer) drain() {
	for len(s.pending) >= waylandHeaderSize {
		word := binary.NativeEndian.Uint32(s.pending[4:])
		size := int(word >> 16)
		if size < waylandHeaderSize || len(s.pending) < size {
			return
		}
		s.handle(binary.LittleEndian.Uint32(s.pending[:4]), word&0xffff, s.pending[waylandHeaderSize:size])
		s.pending = s.pending[size:]
	}
}

func (s *waylandWriteServer) handle(object, opcode uint32, body []byte) {
	if object == waylandDisplayID {
		s.handleDisplay(opcode, body)
		return
	}
	s.handleOffer(object, opcode, body)
}

func (s *waylandWriteServer) handleDisplay(opcode uint32, body []byte) {
	switch opcode {
	case waylandDisplayGetRegistry:
		s.greet(binary.LittleEndian.Uint32(body))
	case waylandDisplaySync:
		s.ack(binary.LittleEndian.Uint32(body))
	}
}

func (s *waylandWriteServer) handleOffer(object, opcode uint32, body []byte) {
	switch object {
	case waylandManagerID:
		s.handleManager(opcode, body)
	case waylandDeviceID:
		if opcode == waylandDeviceSetSelection {
			s.capture.selected = binary.LittleEndian.Uint32(body)
			s.capture.note("select")
		}
	default:
		if object == s.source && opcode == waylandSourceOffer {
			s.offer(body)
		}
	}
}

func (s *waylandWriteServer) handleManager(opcode uint32, body []byte) {
	switch opcode {
	case waylandManagerCreateSource:
		s.source = binary.LittleEndian.Uint32(body)
		s.capture.source = s.source
		s.capture.note("create")
	case waylandManagerGetDevice:
		s.capture.note("device")
	}
}

func (s *waylandWriteServer) greet(registry uint32) {
	s.registry = registry
	_, _ = s.conn.Write(fakeWaylandEvent(registry, 0, uint32(2), "wl_seat", uint32(1)))
	_, _ = s.conn.Write(fakeWaylandEvent(registry, 0, uint32(4), "ext_data_control_manager_v1", uint32(1)))
}

func (s *waylandWriteServer) ack(done uint32) {
	_, _ = s.conn.Write(fakeWaylandEvent(done, 0, uint32(0)))
	if done == waylandWriteDoneID {
		s.synced = true
	}
}

func (s *waylandWriteServer) offer(body []byte) {
	mime, _ := fakeString(body)
	s.capture.mu.Lock()
	s.capture.offers = append(s.capture.offers, mime)
	s.capture.mu.Unlock()
	s.capture.note("offer")
}

func emitWaylandSend(t *testing.T, server *net.UnixConn, source uint32, png []byte, capture *waylandWriteCapture) {
	t.Helper()
	received, send, err := os.Pipe()
	if err != nil {
		t.Error(err)
		return
	}
	defer received.Close()
	event := fakeWaylandEvent(source, waylandSourceSend, imagePNGMIME)
	if _, _, err := server.WriteMsgUnix(event, syscall.UnixRights(int(send.Fd())), nil); err != nil {
		_ = send.Close()
		t.Error(err)
		return
	}
	_ = send.Close()
	var out bytes.Buffer
	if _, err := out.ReadFrom(received); err != nil {
		t.Error(err)
		return
	}
	capture.mu.Lock()
	capture.received = out.Bytes()
	capture.mu.Unlock()
}

func TestWaylandClipboardWritesImageThroughDataSource(t *testing.T) {
	png := testPNG(t, 4, 3)
	client, server := waylandWritePair(t)
	capture := &waylandWriteCapture{}
	done := make(chan struct{})
	go (&waylandWriteServer{t: t, conn: server, capture: capture, png: png}).run(done)
	clipboard := &WaylandClipboard{dial: func() (*net.UnixConn, error) { return client, nil }}
	t.Cleanup(clipboard.CloseImageClipboard)
	publishTestPNG(t, clipboard.ImageClipboard(t.Context()), image.Pt(4, 3), png)
	select {
	case <-done:
	case <-time.After(fakeWaylandDeadline):
		t.Fatal("the compositor did not receive the image offer")
	}
	checkWaylandWrite(t, capture, png)
}

func checkWaylandWrite(t *testing.T, capture *waylandWriteCapture, png []byte) {
	t.Helper()
	capture.mu.Lock()
	defer capture.mu.Unlock()
	if len(capture.offers) != 1 || capture.offers[0] != imagePNGMIME {
		t.Fatalf("offered MIME types %q, want only image/png", capture.offers)
	}
	if capture.selected == 0 || capture.selected != capture.source {
		t.Fatalf("selected source %#x, want the created source %#x", capture.selected, capture.source)
	}
	if !bytes.Equal(capture.received, png) {
		t.Fatalf("the compositor received %d bytes, want the %d offered PNG bytes", len(capture.received), len(png))
	}
	if !slices.Equal(capture.sequence, []string{"create", "device", "offer", "select"}) {
		t.Fatalf("request sequence %q, want one create, device, offer, and select", capture.sequence)
	}
}

func TestWaylandClipboardRejectsExcessiveImageBeforeDialing(t *testing.T) {
	clipboard := &WaylandClipboard{dial: func() (*net.UnixConn, error) {
		t.Error("staging an oversized image reached the compositor")
		return nil, errWaylandProtocol
	}}
	offer := clipboard.ImageClipboard(t.Context())
	defer offer.Close()
	if _, _, err := offer.Stage(clipboardimage.Spec{Size: image.Pt(2, 2), PNGBytes: clipboardimage.MaxBytes + 1}); !errors.Is(err, clipboardimage.ErrBounds) {
		t.Fatalf("staging an oversized image: %v", err)
	}
}
