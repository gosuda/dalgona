//go:build linux || freebsd

package platform

import (
	"bufio"
	"cmp"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"syscall"
	"time"
)

const (
	waylandSetupTimeout = 10 * time.Second
	waylandBufferSize   = 4096
	waylandHeaderSize   = 8
	waylandBindVersion  = 1
)

var waylandTransferTimeout = 30 * time.Second

const (
	waylandDisplayID uint32 = iota + 1
	waylandRegistryID
	waylandGlobalsDoneID
	waylandSeatID
	waylandManagerID
	waylandDeviceID
	waylandSelectionDoneID
)

const (
	waylandDisplaySync        = 0
	waylandDisplayGetRegistry = 1
	waylandDisplayError       = 0
	waylandRegistryBind       = 0
	waylandRegistryGlobal     = 0
	waylandManagerGetDevice   = 1
	waylandDeviceDataOffer    = 0
	waylandDeviceSelection    = 1
	waylandDeviceFinished     = 2
	waylandOfferReceive       = 0
	waylandOfferOffer         = 0
)

var (
	ErrNoDataControl        = errors.New("the Wayland compositor offers no ext-data-control-v1 or wlr-data-control clipboard; run dalgona with XWayland and DISPLAY set, or use a compositor that offers one of them")
	errWaylandProtocol      = errors.New("the Wayland compositor sent a malformed message; paste again, and restart the session if it repeats")
	errWaylandAccessEnded   = errors.New("the Wayland compositor ended clipboard access before the paste finished; paste again")
	errWaylandNoRuntimeDir  = errors.New("XDG_RUNTIME_DIR is not set, so the Wayland socket cannot be found; start dalgona inside the desktop session")
	waylandDataControlNames = [...]string{"ext_data_control_manager_v1", "zwlr_data_control_manager_v1"}
	waylandPlainSlot        = slices.IndexFunc(x11Targets[:], func(target x11Target) bool {
		return strings.EqualFold(target.name, "text/plain;charset=utf-8")
	})
)

type WaylandClipboard struct {
	dial  func() (*net.UnixConn, error)
	mu    sync.Mutex
	owner *waylandImageOwner
}

type waylandConn struct {
	conn *net.UnixConn
	in   *bufio.Reader
	body []byte
}

type waylandEvent struct {
	sender uint32
	opcode uint32
	args   []byte
}

type waylandArgs struct {
	rest []byte
	err  error
}

type waylandRequest []byte

type waylandGlobals struct {
	seat    uint32
	hasSeat bool
	manager uint32
	iface   string
}

type waylandOffer struct {
	id    uint32
	mimes [len(x11Targets)]string
}

func OpenWaylandClipboard() (*WaylandClipboard, error) {
	socket, err := waylandSocket()
	if err != nil {
		return nil, err
	}
	c := &WaylandClipboard{dial: func() (*net.UnixConn, error) {
		return net.DialUnix("unix", nil, &net.UnixAddr{Name: socket, Net: "unix"})
	}}
	if err := c.probe(); err != nil {
		return nil, err
	}
	return c, nil
}

func waylandSocket() (string, error) {
	name := cmp.Or(os.Getenv("WAYLAND_DISPLAY"), "wayland-0")
	if filepath.IsAbs(name) {
		return name, nil
	}
	dir := os.Getenv("XDG_RUNTIME_DIR")
	if dir == "" {
		return "", errWaylandNoRuntimeDir
	}
	return filepath.Join(dir, name), nil
}

func (c *WaylandClipboard) connect() (*waylandConn, error) {
	conn, err := c.dial()
	if err != nil {
		return nil, fmt.Errorf("connect to the Wayland compositor: %w; start dalgona inside the desktop session", err)
	}
	return &waylandConn{conn: conn, in: bufio.NewReaderSize(conn, waylandBufferSize)}, nil
}

func (c *WaylandClipboard) probe() error {
	w, err := c.connect()
	if err != nil {
		return err
	}
	err = w.conn.SetDeadline(time.Now().Add(waylandSetupTimeout))
	if err == nil {
		_, err = w.globals()
	}
	return errors.Join(err, w.conn.Close())
}

func (c *WaylandClipboard) ReadClipboard(ctx context.Context, kind PasteKind) (ClipboardContent, error) {
	limited, cancel := context.WithTimeout(ctx, waylandTransferTimeout)
	defer cancel()
	ctx = limited
	w, err := c.connect()
	if err != nil {
		return ClipboardContent{}, err
	}
	defer func() { _ = w.conn.Close() }()
	pipe, source, err := openTransferPipe()
	if err != nil {
		return ClipboardContent{}, err
	}
	defer func() { _ = pipe.Close() }()
	stop := context.AfterFunc(ctx, func() {
		expired := time.Unix(1, 0)
		_ = w.conn.SetDeadline(expired)
		_ = pipe.SetReadDeadline(expired)
	})
	defer stop()
	chosen, err := w.requestSelection(source, kind)
	if closeErr := syscall.Close(source); err == nil && closeErr != nil {
		err = fmt.Errorf("close the clipboard transfer pipe: %w", closeErr)
	}
	var data []byte
	if err == nil {
		data, err = readTransfer(pipe)
	}
	if err != nil {
		return ClipboardContent{}, waylandPasteError(ctx, err)
	}
	target := x11Targets[chosen]
	return target.decode(target.name, data)
}

func waylandPasteError(ctx context.Context, err error) error {
	if ctx.Err() != nil && errors.Is(err, os.ErrDeadlineExceeded) {
		return fmt.Errorf("the clipboard owner did not answer before the paste ended; paste again, or copy the content again if its application is busy: %w", context.Cause(ctx))
	}
	return err
}

func openTransferPipe() (*os.File, int, error) {
	var fds [2]int
	if err := syscall.Pipe2(fds[:], syscall.O_CLOEXEC); err != nil {
		return nil, -1, fmt.Errorf("open the clipboard transfer pipe: %w", err)
	}
	if err := syscall.SetNonblock(fds[0], true); err != nil {
		return nil, -1, errors.Join(fmt.Errorf("prepare the clipboard transfer pipe: %w", err), syscall.Close(fds[0]), syscall.Close(fds[1]))
	}
	return os.NewFile(uintptr(fds[0]), "wayland-clipboard"), fds[1], nil
}

func readTransfer(r io.Reader) ([]byte, error) {
	data, err := io.ReadAll(io.LimitReader(r, transferLimit+1))
	if err != nil {
		return nil, fmt.Errorf("read the clipboard transfer: %w", err)
	}
	if len(data) > transferLimit {
		return nil, ErrTransferTooLarge
	}
	return data, nil
}

func (w *waylandConn) requestSelection(source int, kind PasteKind) (int, error) {
	globals, err := w.globals()
	if err != nil {
		return 0, err
	}
	offer, err := w.selection(globals)
	if err != nil {
		return 0, err
	}
	chosen, err := offer.choose(kind)
	if err != nil {
		return 0, err
	}
	receive := newWaylandRequest(offer.id, waylandOfferReceive).string(offer.mimes[chosen]).bytes()
	n, _, err := w.conn.WriteMsgUnix(receive, syscall.UnixRights(source), nil)
	if err == nil && n != len(receive) {
		err = io.ErrShortWrite
	}
	if err != nil {
		return 0, fmt.Errorf("ask the Wayland compositor for the clipboard: %w", err)
	}
	return chosen, nil
}

func (w *waylandConn) globals() (waylandGlobals, error) {
	err := w.send(
		newWaylandRequest(waylandDisplayID, waylandDisplayGetRegistry).uint(waylandRegistryID),
		newWaylandRequest(waylandDisplayID, waylandDisplaySync).uint(waylandGlobalsDoneID),
	)
	var g waylandGlobals
	for err == nil {
		var ev waylandEvent
		if ev, err = w.next(); err != nil {
			break
		}
		switch ev.sender {
		case waylandRegistryID:
			err = g.note(ev)
		case waylandGlobalsDoneID:
			return g, g.check()
		}
	}
	return g, err
}

func (g *waylandGlobals) note(ev waylandEvent) error {
	if ev.opcode != waylandRegistryGlobal {
		return nil
	}
	a := waylandArgs{rest: ev.args}
	name, iface, _ := a.uint(), a.string(), a.uint()
	if a.err != nil {
		return a.err
	}
	rank := slices.Index(waylandDataControlNames[:], iface)
	switch {
	case iface == "wl_seat" && !g.hasSeat:
		g.seat, g.hasSeat = name, true
	case rank >= 0 && (g.iface == "" || rank < slices.Index(waylandDataControlNames[:], g.iface)):
		g.manager, g.iface = name, iface
	}
	return nil
}

func (g waylandGlobals) check() error {
	if !g.hasSeat {
		return fmt.Errorf("%w; the compositor announces no seat", ErrNoDataControl)
	}
	if g.iface == "" {
		return ErrNoDataControl
	}
	return nil
}

func (w *waylandConn) selection(g waylandGlobals) (waylandOffer, error) {
	err := w.send(
		newWaylandRequest(waylandRegistryID, waylandRegistryBind).uint(g.seat).string("wl_seat").uint(waylandBindVersion).uint(waylandSeatID),
		newWaylandRequest(waylandRegistryID, waylandRegistryBind).uint(g.manager).string(g.iface).uint(waylandBindVersion).uint(waylandManagerID),
		newWaylandRequest(waylandManagerID, waylandManagerGetDevice).uint(waylandDeviceID).uint(waylandSeatID),
		newWaylandRequest(waylandDisplayID, waylandDisplaySync).uint(waylandSelectionDoneID),
	)
	var latest, selected waylandOffer
	for err == nil {
		var ev waylandEvent
		if ev, err = w.next(); err != nil {
			break
		}
		switch {
		case ev.sender == waylandSelectionDoneID:
			return selected, nil
		case ev.sender == waylandDeviceID:
			err = deviceEvent(ev, &latest, &selected)
		case ev.sender == latest.id && latest.id != 0:
			err = latest.note(ev)
		}
	}
	return waylandOffer{}, err
}

func deviceEvent(ev waylandEvent, latest, selected *waylandOffer) error {
	a := waylandArgs{rest: ev.args}
	switch ev.opcode {
	case waylandDeviceDataOffer:
		*latest = waylandOffer{id: a.uint()}
	case waylandDeviceSelection:
		*selected = waylandOffer{}
		if id := a.uint(); id != 0 && id == latest.id {
			*selected = *latest
		}
	case waylandDeviceFinished:
		return errWaylandAccessEnded
	}
	return a.err
}

func (o *waylandOffer) note(ev waylandEvent) error {
	if ev.opcode != waylandOfferOffer {
		return nil
	}
	a := waylandArgs{rest: ev.args}
	mime := a.string()
	for i, target := range x11Targets {
		if o.mimes[i] == "" && len(mime) == len(target.name) && strings.EqualFold(mime, target.name) {
			o.mimes[i] = mime
		}
	}
	if waylandPlainSlot >= 0 && o.mimes[waylandPlainSlot] == "" {
		if plain, ok := waylandPlainText(mime); ok {
			o.mimes[waylandPlainSlot] = plain
		}
	}
	return a.err
}

func waylandPlainText(mime string) (string, bool) {
	switch strings.ToLower(strings.TrimSpace(mime)) {
	case "text/plain", "text/plain;charset=utf8":
		return mime, true
	}
	return "", false
}

func (o waylandOffer) choose(kind PasteKind) (int, error) {
	var available ClipboardFormats
	for i, mime := range o.mimes {
		if mime != "" {
			available |= x11Targets[i].format
		}
	}
	format, err := ChoosePaste(available, kind)
	if err != nil {
		return 0, err
	}
	for i, mime := range o.mimes {
		if mime != "" && x11Targets[i].format == format {
			return i, nil
		}
	}
	return 0, ErrUnsupportedPaste
}

func (w *waylandConn) send(requests ...waylandRequest) error {
	var out []byte
	for _, r := range requests {
		out = append(out, r.bytes()...)
	}
	if _, err := w.conn.Write(out); err != nil {
		return fmt.Errorf("send to the Wayland compositor: %w", err)
	}
	return nil
}

func (w *waylandConn) next() (waylandEvent, error) {
	for {
		ev, err := w.read()
		if err != nil || ev.sender != waylandDisplayID {
			return ev, err
		}
		if ev.opcode == waylandDisplayError {
			return waylandEvent{}, displayError(ev)
		}
	}
}

func (w *waylandConn) read() (waylandEvent, error) {
	var head [waylandHeaderSize]byte
	if _, err := io.ReadFull(w.in, head[:]); err != nil {
		return waylandEvent{}, fmt.Errorf("read from the Wayland compositor: %w", err)
	}
	word := binary.NativeEndian.Uint32(head[4:])
	size := int(word >> 16)
	if size < waylandHeaderSize || size%4 != 0 {
		return waylandEvent{}, errWaylandProtocol
	}
	w.body = slices.Grow(w.body[:0], size-waylandHeaderSize)[:size-waylandHeaderSize]
	if _, err := io.ReadFull(w.in, w.body); err != nil {
		return waylandEvent{}, fmt.Errorf("read from the Wayland compositor: %w", err)
	}
	return waylandEvent{sender: binary.NativeEndian.Uint32(head[:4]), opcode: word & 0xffff, args: w.body}, nil
}

func displayError(ev waylandEvent) error {
	a := waylandArgs{rest: ev.args}
	object, code, message := a.uint(), a.uint(), a.string()
	if a.err != nil {
		return a.err
	}
	return fmt.Errorf("the Wayland compositor refused the paste with error %d on object %d (%s); paste again", code, object, message)
}

func (a *waylandArgs) uint() uint32 {
	if len(a.rest) < 4 {
		a.err = errWaylandProtocol
		return 0
	}
	v := binary.NativeEndian.Uint32(a.rest)
	a.rest = a.rest[4:]
	return v
}

func (a *waylandArgs) string() string {
	n := uint64(a.uint())
	padded := (n + 3) &^ 3
	if n == 0 || padded > uint64(len(a.rest)) || a.rest[n-1] != 0 {
		a.err = cmp.Or(a.err, errWaylandProtocol)
		return ""
	}
	text := string(a.rest[:n-1])
	a.rest = a.rest[padded:]
	return text
}

func newWaylandRequest(object, opcode uint32) waylandRequest {
	r := make(waylandRequest, waylandHeaderSize, 64)
	binary.NativeEndian.PutUint32(r, object)
	binary.NativeEndian.PutUint32(r[4:], opcode)
	return r
}

func (r waylandRequest) uint(v uint32) waylandRequest {
	return binary.NativeEndian.AppendUint32(r, v)
}

func (r waylandRequest) string(s string) waylandRequest {
	r = r.uint(uint32(len(s) + 1)) //nolint:gosec // requests carry interface names and known MIME spellings, all shorter than 64 bytes
	r = append(r, s...)
	return append(r, make([]byte, 4-len(s)%4)...)
}

func (r waylandRequest) bytes() []byte {
	binary.NativeEndian.PutUint32(r[4:], uint32(len(r))<<16|binary.NativeEndian.Uint32(r[4:])) //nolint:gosec // each request stays under 200 bytes, so its size fits the 16-bit field
	return r
}
