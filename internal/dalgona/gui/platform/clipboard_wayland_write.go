//go:build linux || freebsd

package platform

import (
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"os"
	"syscall"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const (
	waylandManagerCreateSource = 0
	waylandSourceOffer         = 0
	waylandDeviceSetSelection  = 0
	waylandSourceSend          = 0
	waylandSourceCancelled     = 1
	waylandSourceID            = 8
	waylandWriteDoneID         = 9
	waylandOwnerBufferSize     = 65536
	waylandOwnerRightsSize     = 16
)

type waylandImageOwner struct {
	conn    *net.UnixConn
	pending []byte
	fds     []int
	source  uint32
	png     []byte
}

func (c *WaylandClipboard) ImageClipboard(ctx context.Context) clipboardimage.Offer {
	return NewPNGClipboardOffer(ctx, c.writeImage)
}

func (c *WaylandClipboard) CloseImageClipboard() {
	c.mu.Lock()
	owner := c.owner
	c.owner = nil
	c.mu.Unlock()
	if owner != nil {
		_ = owner.conn.Close()
	}
}

func (c *WaylandClipboard) writeImage(ctx context.Context, png []byte) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	owner, err := serveWaylandImage(c.dial, png)
	if err != nil {
		return err
	}
	c.mu.Lock()
	previous := c.owner
	c.owner = owner
	c.mu.Unlock()
	if previous != nil {
		_ = previous.conn.Close()
	}
	return nil
}

func serveWaylandImage(dial func() (*net.UnixConn, error), png []byte) (*waylandImageOwner, error) {
	conn, err := dial()
	if err != nil {
		return nil, fmt.Errorf("connect to the Wayland compositor: %w; start dalgona inside the desktop session", err)
	}
	owner := &waylandImageOwner{conn: conn, source: waylandSourceID, png: png}
	if err := owner.publish(); err != nil {
		_ = conn.Close()
		return nil, err
	}
	go owner.serve()
	return owner, nil
}

func (o *waylandImageOwner) publish() error {
	if err := o.conn.SetDeadline(time.Now().Add(waylandSetupTimeout)); err != nil {
		return fmt.Errorf("bound the Wayland clipboard offer time: %w; try again", err)
	}
	defer func() { _ = o.conn.SetDeadline(time.Time{}) }()
	if err := o.send(
		newWaylandRequest(waylandDisplayID, waylandDisplayGetRegistry).uint(waylandRegistryID),
		newWaylandRequest(waylandDisplayID, waylandDisplaySync).uint(waylandGlobalsDoneID),
	); err != nil {
		return err
	}
	globals, err := o.awaitGlobals()
	if err != nil {
		return err
	}
	return o.offer(globals)
}

func (o *waylandImageOwner) awaitGlobals() (waylandGlobals, error) {
	var globals waylandGlobals
	for {
		ev, fd, err := o.next()
		closeWaylandFD(fd)
		if err != nil {
			return waylandGlobals{}, err
		}
		switch ev.sender {
		case waylandRegistryID:
			if err := globals.note(ev); err != nil {
				return waylandGlobals{}, err
			}
		case waylandGlobalsDoneID:
			if err := globals.check(); err != nil {
				return waylandGlobals{}, err
			}
			return globals, nil
		}
	}
}

func (o *waylandImageOwner) offer(globals waylandGlobals) error {
	if err := o.send(
		newWaylandRequest(waylandRegistryID, waylandRegistryBind).uint(globals.seat).string("wl_seat").uint(waylandBindVersion).uint(waylandSeatID),
		newWaylandRequest(waylandRegistryID, waylandRegistryBind).uint(globals.manager).string(globals.iface).uint(waylandBindVersion).uint(waylandManagerID),
		newWaylandRequest(waylandManagerID, waylandManagerCreateSource).uint(waylandSourceID),
		newWaylandRequest(waylandManagerID, waylandManagerGetDevice).uint(waylandDeviceID).uint(waylandSeatID),
		newWaylandRequest(waylandSourceID, waylandSourceOffer).string(imagePNGMIME),
		newWaylandRequest(waylandDeviceID, waylandDeviceSetSelection).uint(waylandSourceID),
		newWaylandRequest(waylandDisplayID, waylandDisplaySync).uint(waylandWriteDoneID),
	); err != nil {
		return err
	}
	for {
		ev, fd, err := o.next()
		closeWaylandFD(fd)
		if err != nil {
			return err
		}
		if ev.sender == waylandWriteDoneID {
			return nil
		}
	}
}

func (o *waylandImageOwner) send(requests ...waylandRequest) error {
	var out []byte
	for _, r := range requests {
		out = append(out, r.bytes()...)
	}
	if _, err := o.conn.Write(out); err != nil {
		return fmt.Errorf("send to the Wayland compositor: %w", err)
	}
	return nil
}

func (o *waylandImageOwner) next() (waylandEvent, int, error) {
	ev, err := o.receive()
	if err != nil {
		return waylandEvent{}, -1, err
	}
	if ev.sender == waylandDisplayID && ev.opcode == waylandDisplayError {
		return waylandEvent{}, -1, displayError(ev)
	}
	return ev, o.takeSendFD(ev), nil
}

func (o *waylandImageOwner) receive() (waylandEvent, error) {
	for {
		ev, size, err := splitWaylandOwnerMessage(o.pending)
		if err != nil {
			return waylandEvent{}, err
		}
		if size > 0 {
			o.pending = o.pending[size:]
			return ev, nil
		}
		if err := o.fill(); err != nil {
			return waylandEvent{}, err
		}
	}
}

func (o *waylandImageOwner) takeSendFD(ev waylandEvent) int {
	if ev.sender != o.source || ev.opcode != waylandSourceSend || len(o.fds) == 0 {
		return -1
	}
	fd := o.fds[0]
	o.fds = o.fds[1:]
	return fd
}

func splitWaylandOwnerMessage(pending []byte) (waylandEvent, int, error) {
	if len(pending) < waylandHeaderSize {
		return waylandEvent{}, 0, nil
	}
	word := binary.NativeEndian.Uint32(pending[4:])
	size := int(word >> 16)
	if size < waylandHeaderSize || size%4 != 0 {
		return waylandEvent{}, 0, errWaylandProtocol
	}
	if len(pending) < size {
		return waylandEvent{}, 0, nil
	}
	return waylandEvent{sender: binary.LittleEndian.Uint32(pending[:4]), opcode: word & 0xffff, args: pending[waylandHeaderSize:size]}, size, nil
}

func (o *waylandImageOwner) fill() error {
	data := make([]byte, waylandOwnerBufferSize)
	oob := make([]byte, syscall.CmsgSpace(waylandOwnerRightsSize))
	n, oobn, _, _, err := o.conn.ReadMsgUnix(data, oob)
	if err == nil && n == 0 {
		err = io.ErrUnexpectedEOF
	}
	if err != nil {
		return fmt.Errorf("read from the Wayland compositor: %w", err)
	}
	o.pending = append(o.pending, data[:n]...)
	o.fds = append(o.fds, waylandMessageFDs(oob[:oobn])...)
	return nil
}

func (o *waylandImageOwner) serve() {
	defer o.conn.Close()
	for {
		ev, fd, err := o.next()
		if err != nil {
			closeWaylandFD(fd)
			return
		}
		if ev.sender != o.source {
			closeWaylandFD(fd)
			continue
		}
		switch ev.opcode {
		case waylandSourceSend:
			o.answer(ev, fd)
		case waylandSourceCancelled:
			closeWaylandFD(fd)
			return
		default:
			closeWaylandFD(fd)
		}
	}
}

func (o *waylandImageOwner) answer(ev waylandEvent, fd int) {
	if fd < 0 {
		return
	}
	file := os.NewFile(uintptr(fd), "wayland-clipboard-image") //nolint:gosec // G115: the compositor passes a non-negative descriptor; negative values return above
	args := waylandArgs{rest: ev.args}
	if mime := args.string(); args.err != nil || mime != imagePNGMIME {
		_ = file.Close()
		return
	}
	writeWaylandImage(file, o.png)
	_ = file.Close()
}

func writeWaylandImage(file *os.File, png []byte) {
	for len(png) > 0 {
		n, err := file.Write(png)
		png = png[n:]
		if err != nil || n == 0 {
			return
		}
	}
}

func closeWaylandFD(fd int) {
	if fd >= 0 {
		_ = syscall.Close(fd)
	}
}

func waylandMessageFDs(oob []byte) []int {
	messages, err := syscall.ParseSocketControlMessage(oob)
	if err != nil {
		return nil
	}
	var fds []int
	for _, message := range messages {
		rights, err := syscall.ParseUnixRights(&message)
		if err == nil {
			fds = append(fds, rights...)
		}
	}
	return fds
}
