//go:build linux || freebsd || openbsd

package platform

import (
	"context"
	"encoding/binary"
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const (
	x11OpChangeWindowAttributes = 2
	x11OpSetSelectionOwner      = 22
	x11OpGetSelectionOwner      = 23
	x11OpSendEvent              = 25
	x11EventSelectionClear      = 29
	x11EventSelectionRequest    = 30
	x11PropertyDeleted          = 1
	x11ModeReplace              = 0
)

type x11ImageOwner struct {
	conn      *x11Conn
	window    uint32
	clipboard uint32
	targets   uint32
	image     uint32
	incr      uint32
	png       []byte
}

type x11ServeRequest struct {
	time      uint32
	requestor uint32
	selection uint32
	target    uint32
	property  uint32
}

func (c *X11Clipboard) ImageClipboard(ctx context.Context) clipboardimage.Offer {
	return NewPNGClipboardOffer(ctx, c.writeImage)
}

func (c *X11Clipboard) writeImage(ctx context.Context, png []byte) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	owner, err := serveX11Image(c.display, png)
	if err != nil {
		return err
	}
	c.owning.Lock()
	previous := c.owner
	c.owner = owner
	c.owning.Unlock()
	if previous != nil {
		_ = previous.conn.close()
	}
	return nil
}

func serveX11Image(display string, png []byte) (*x11ImageOwner, error) {
	conn, err := dialX11(display)
	if err != nil {
		return nil, err
	}
	owner := &x11ImageOwner{conn: conn, png: png}
	if err := owner.own(); err != nil {
		_ = conn.close()
		return nil, err
	}
	go owner.serve()
	return owner, nil
}

func (o *x11ImageOwner) own() error {
	atoms, err := o.conn.internAtoms([]string{"CLIPBOARD", "TARGETS", "INCR", imagePNGMIME})
	if err != nil {
		return err
	}
	o.clipboard, o.targets, o.incr, o.image = atoms[0], atoms[1], atoms[2], atoms[3]
	window, err := o.conn.createInputWindow(0)
	if err != nil {
		return err
	}
	o.window = window
	if err := o.claim(); err != nil {
		return err
	}
	return o.verify()
}

func (o *x11ImageOwner) claim() error {
	request := binary.LittleEndian.AppendUint32(o.conn.begin(x11OpSetSelectionOwner, 0), o.window)
	request = binary.LittleEndian.AppendUint32(request, o.clipboard)
	return o.conn.send(binary.LittleEndian.AppendUint32(request, 0))
}

func (o *x11ImageOwner) verify() error {
	reply, err := o.conn.roundTrip(binary.LittleEndian.AppendUint32(o.conn.begin(x11OpGetSelectionOwner, 0), o.clipboard))
	if err != nil {
		return err
	}
	if current := binary.LittleEndian.Uint32(reply[8:]); current != o.window {
		return fmt.Errorf("offer the PNG image on the X11 clipboard: another application owns the clipboard; copy the image again")
	}
	return nil
}

func (o *x11ImageOwner) serve() {
	defer o.conn.close()
	for {
		event, err := o.conn.nextEvent()
		if err != nil {
			return
		}
		if event[0]&x11EventCodeMask == x11EventSelectionRequest {
			_ = o.answer(parseX11ServeRequest(event))
		}
	}
}

func parseX11ServeRequest(event [32]byte) x11ServeRequest {
	return x11ServeRequest{
		time:      binary.LittleEndian.Uint32(event[4:]),
		requestor: binary.LittleEndian.Uint32(event[12:]),
		selection: binary.LittleEndian.Uint32(event[16:]),
		target:    binary.LittleEndian.Uint32(event[20:]),
		property:  binary.LittleEndian.Uint32(event[24:]),
	}
}

func (o *x11ImageOwner) answer(r x11ServeRequest) error {
	switch {
	case r.selection != o.clipboard || r.property == 0:
		return nil
	case r.target == o.targets:
		return o.store(r, o.targets, 32, o.targetList())
	case r.target == o.image:
		return o.storeImage(r)
	default:
		return o.notify(r, 0)
	}
}

func (o *x11ImageOwner) targetList() []byte {
	list := binary.LittleEndian.AppendUint32(nil, o.targets)
	return binary.LittleEndian.AppendUint32(list, o.image)
}

func (o *x11ImageOwner) storeImage(r x11ServeRequest) error {
	if len(o.png) <= o.chunkSize() {
		return o.store(r, o.image, 8, o.png)
	}
	return o.sendIncremental(r)
}

func (o *x11ImageOwner) chunkSize() int {
	return 4*o.conn.maxRequestUnits - 24
}

func (o *x11ImageOwner) store(r x11ServeRequest, kind uint32, format byte, data []byte) error {
	write := x11PropertyWrite{window: r.requestor, property: r.property, kind: kind, mode: x11ModeReplace, format: format}
	for chunk := range slices.Chunk(data, o.chunkSize()) {
		write.data = chunk
		if err := o.conn.changeProperty(write); err != nil {
			return err
		}
		write.mode = x11ModeAppend
	}
	return o.notify(r, r.property)
}

func (o *x11ImageOwner) sendIncremental(r x11ServeRequest) error {
	if err := o.watchDeletions(r.requestor); err != nil {
		return err
	}
	size := binary.LittleEndian.AppendUint32(nil, uint32(len(o.png))) //nolint:gosec // The verified export is at most clipboardimage.MaxBytes.
	if err := o.conn.changeProperty(x11PropertyWrite{window: r.requestor, property: r.property, kind: o.incr, mode: x11ModeReplace, format: 32, data: size}); err != nil {
		return err
	}
	if err := o.notify(r, r.property); err != nil {
		return err
	}
	write := x11PropertyWrite{window: r.requestor, property: r.property, kind: o.image, mode: x11ModeAppend, format: 8}
	for chunk := range slices.Chunk(o.png, o.chunkSize()) {
		write.data = chunk
		if err := o.awaitDeletion(r); err != nil {
			return err
		}
		if err := o.conn.changeProperty(write); err != nil {
			return err
		}
	}
	if err := o.awaitDeletion(r); err != nil {
		return err
	}
	write.data = nil
	return o.conn.changeProperty(write)
}

func (o *x11ImageOwner) watchDeletions(window uint32) error {
	request := binary.LittleEndian.AppendUint32(o.conn.begin(x11OpChangeWindowAttributes, 0), window)
	request = binary.LittleEndian.AppendUint32(request, x11ValueEventMask)
	return o.conn.send(binary.LittleEndian.AppendUint32(request, x11PropertyChangeMask))
}

func (o *x11ImageOwner) awaitDeletion(r x11ServeRequest) error {
	for {
		event, err := o.conn.nextEvent()
		if err != nil {
			return err
		}
		switch event[0] & x11EventCodeMask {
		case x11EventSelectionRequest:
			_ = o.answer(parseX11ServeRequest(event))
		case x11EventPropertyNotify:
			if event[16] == x11PropertyDeleted && binary.LittleEndian.Uint32(event[4:]) == r.requestor && binary.LittleEndian.Uint32(event[8:]) == r.property {
				return nil
			}
		}
	}
}

func (o *x11ImageOwner) notify(r x11ServeRequest, property uint32) error {
	request := binary.LittleEndian.AppendUint32(o.conn.begin(x11OpSendEvent, 0), r.requestor)
	request = binary.LittleEndian.AppendUint32(request, 0)
	request = append(request, x11EventSelectionNotify, 0, 0, 0)
	for _, field := range [...]uint32{r.time, r.requestor, r.selection, r.target, property} {
		request = binary.LittleEndian.AppendUint32(request, field)
	}
	return o.conn.send(append(request, 0, 0, 0, 0, 0, 0, 0, 0))
}
