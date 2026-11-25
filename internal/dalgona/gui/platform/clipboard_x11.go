//go:build linux || freebsd || openbsd

package platform

import (
	"bytes"
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"os"
	"slices"
	"strings"
	"sync"
)

type x11Target struct {
	name   string
	format ClipboardFormats
	decode func(target string, data []byte) (ClipboardContent, error)
}

var x11Targets = [...]x11Target{
	{name: "text/uri-list", format: ClipboardFiles, decode: decodeX11URIList},
	{name: "x-special/gnome-copied-files", format: ClipboardFiles, decode: decodeX11CopiedFiles},
	{name: "UTF8_STRING", format: ClipboardText, decode: decodeX11Text},
	{name: "text/plain;charset=utf-8", format: ClipboardText, decode: decodeX11Text},
	{name: "image/png", format: ClipboardImage, decode: decodeX11Image},
	{name: "image/jpeg", format: ClipboardImage, decode: decodeX11Image},
}

type X11Clipboard struct {
	pasting    sync.Mutex
	selection  *x11Conn
	measuring  sync.Mutex
	geometry   *x11Conn
	owning     sync.Mutex
	owner      *x11ImageOwner
	display    string
	clipboard  uint32
	targetList uint32
	incr       uint32
	property   uint32
	targets    [len(x11Targets)]uint32
}

func OpenX11Clipboard(display string) (*X11Clipboard, error) {
	selection, err := dialX11(display)
	if err != nil {
		return nil, err
	}
	geometry, err := dialX11(display)
	if err != nil {
		return nil, errors.Join(err, selection.close())
	}
	c := &X11Clipboard{selection: selection, geometry: geometry, display: display}
	names := []string{"CLIPBOARD", "TARGETS", "INCR", "DALGONA_PASTE"}
	for _, target := range x11Targets {
		names = append(names, target.name)
	}
	atoms, err := selection.internAtoms(names)
	if err != nil {
		return nil, errors.Join(err, c.Close())
	}
	c.clipboard, c.targetList, c.incr, c.property = atoms[0], atoms[1], atoms[2], atoms[3]
	copy(c.targets[:], atoms[4:])
	return c, nil
}

func (c *X11Clipboard) Close() error {
	return errors.Join(c.selection.close(), c.geometry.close(), c.retireOwner())
}

func (c *X11Clipboard) retireOwner() error {
	c.owning.Lock()
	owner := c.owner
	c.owner = nil
	c.owning.Unlock()
	if owner == nil {
		return nil
	}
	return owner.conn.close()
}

func (c *X11Clipboard) WindowOrigin(window uint32) (image.Point, error) {
	c.measuring.Lock()
	defer c.measuring.Unlock()
	return c.geometry.windowOrigin(window)
}

func (c *X11Clipboard) ReadClipboard(ctx context.Context, kind PasteKind) (ClipboardContent, error) {
	c.pasting.Lock()
	defer c.pasting.Unlock()
	c.selection.mark()
	window, err := c.selection.createInputWindow(x11PropertyChangeMask)
	if err != nil {
		return ClipboardContent{}, err
	}
	stop := c.selection.interruptOn(ctx)
	content, err := c.paste(window, kind)
	stop()
	if ctx.Err() != nil && errors.Is(err, os.ErrDeadlineExceeded) {
		err = fmt.Errorf("the clipboard owner did not answer before the paste ended; paste again, or copy the content again if its application is busy: %w", ctx.Err())
	}
	if destroyed := c.selection.destroyWindow(window); err == nil {
		err = destroyed
	}
	if err != nil {
		return ClipboardContent{}, err
	}
	return content, nil
}

func (c *X11Clipboard) paste(window uint32, kind PasteKind) (ClipboardContent, error) {
	when, err := c.timestamp(window)
	if err != nil {
		return ClipboardContent{}, err
	}
	offered, err := c.convert(window, c.targetList, when)
	if err != nil {
		return ClipboardContent{}, err
	}
	chosen, err := c.choose(offered, kind)
	if err != nil {
		return ClipboardContent{}, err
	}
	data, err := c.convert(window, c.targets[chosen], when)
	if err != nil {
		return ClipboardContent{}, err
	}
	if data.format != 8 {
		return ClipboardContent{}, ErrUnsupportedPaste
	}
	target := x11Targets[chosen]
	return target.decode(target.name, data.value)
}

func (c *X11Clipboard) timestamp(window uint32) (uint32, error) {
	err := c.selection.changeProperty(x11PropertyWrite{window: window, property: c.property, kind: x11AtomString, mode: x11ModeAppend, format: 8})
	if err != nil {
		return 0, err
	}
	when, err := c.awaitNewValue(window, c.property)
	if err != nil {
		return 0, err
	}
	return when, c.selection.deleteProperty(window, c.property)
}

func (c *X11Clipboard) awaitNewValue(window, property uint32) (uint32, error) {
	for {
		event, err := c.selection.nextEvent()
		if err != nil {
			return 0, err
		}
		notify := event[0]&x11EventCodeMask == x11EventPropertyNotify && event[16] == x11PropertyNewValue
		if notify && binary.LittleEndian.Uint32(event[4:]) == window && binary.LittleEndian.Uint32(event[8:]) == property {
			return binary.LittleEndian.Uint32(event[12:]), nil
		}
	}
}

func (c *X11Clipboard) awaitSelection(window, target uint32) (uint32, error) {
	for {
		event, err := c.selection.nextEvent()
		if err != nil {
			return 0, err
		}
		notify := event[0]&x11EventCodeMask == x11EventSelectionNotify && binary.LittleEndian.Uint32(event[8:]) == window
		if notify && binary.LittleEndian.Uint32(event[12:]) == c.clipboard && binary.LittleEndian.Uint32(event[16:]) == target {
			return binary.LittleEndian.Uint32(event[20:]), nil
		}
	}
}

func (c *X11Clipboard) convert(window, target, when uint32) (x11Property, error) {
	if err := c.selection.convertSelection(window, c.clipboard, target, c.property, when); err != nil {
		return x11Property{}, err
	}
	property, err := c.awaitSelection(window, target)
	if err != nil {
		return x11Property{}, err
	}
	if property == 0 {
		return x11Property{}, ErrUnsupportedPaste
	}
	reply, err := c.selection.readProperty(window, property, nil)
	if err != nil {
		return x11Property{}, err
	}
	if reply.kind == c.incr {
		return c.readIncremental(window, property, reply)
	}
	return reply, c.selection.deleteProperty(window, property)
}

func (c *X11Clipboard) readIncremental(window, property uint32, announced x11Property) (x11Property, error) {
	if len(announced.value) == 4 && binary.LittleEndian.Uint32(announced.value) > transferLimit {
		return x11Property{}, ErrTransferTooLarge
	}
	if err := c.selection.deleteProperty(window, property); err != nil {
		return x11Property{}, err
	}
	var data x11Property
	for {
		chunk, err := c.readIncrement(window, property, data.value)
		if err != nil {
			return x11Property{}, err
		}
		if err := data.observe(chunk); err != nil {
			return x11Property{}, err
		}
		if len(chunk.value) == len(data.value) {
			return data, nil
		}
		data.value = chunk.value
	}
}

func (p *x11Property) observe(chunk x11Property) error {
	if p.format == 0 {
		p.kind, p.format = chunk.kind, chunk.format
		return nil
	}
	if chunk.kind != p.kind || chunk.format != p.format {
		return errX11Malformed
	}
	return nil
}

func (c *X11Clipboard) readIncrement(window, property uint32, prior []byte) (x11Property, error) {
	if _, err := c.awaitNewValue(window, property); err != nil {
		return x11Property{}, err
	}
	chunk, err := c.selection.readProperty(window, property, prior)
	if err != nil {
		return x11Property{}, err
	}
	return chunk, c.selection.deleteProperty(window, property)
}

func (c *X11Clipboard) choose(offered x11Property, kind PasteKind) (int, error) {
	if offered.format != 32 || len(offered.value)%4 != 0 {
		return 0, ErrUnsupportedPaste
	}
	var present [len(x11Targets)]bool
	var available ClipboardFormats
	for atom := range slices.Chunk(offered.value, 4) {
		if i := slices.Index(c.targets[:], binary.LittleEndian.Uint32(atom)); i >= 0 {
			present[i] = true
			available |= x11Targets[i].format
		}
	}
	format, err := ChoosePaste(available, kind)
	if err != nil {
		return 0, err
	}
	for i, target := range x11Targets {
		if present[i] && target.format == format {
			return i, nil
		}
	}
	return 0, ErrUnsupportedPaste
}

func decodeX11Text(_ string, data []byte) (ClipboardContent, error) {
	text, err := ReadText(bytes.NewReader(data))
	if err != nil {
		return ClipboardContent{}, err
	}
	return ClipboardContent{Text: text}, nil
}

func decodeX11Image(target string, data []byte) (ClipboardContent, error) {
	return ClipboardContent{Image: data, ImageType: target}, nil
}

func decodeX11URIList(_ string, data []byte) (ClipboardContent, error) {
	return x11Files(string(data))
}

func decodeX11CopiedFiles(_ string, data []byte) (ClipboardContent, error) {
	action, list, _ := strings.Cut(string(data), "\n")
	if action != "copy" && action != "cut" {
		return ClipboardContent{}, ErrUnsupportedPaste
	}
	return x11Files(list)
}

func x11Files(list string) (ClipboardContent, error) {
	files, err := ParseURIList(list)
	if err != nil {
		return ClipboardContent{}, err
	}
	return ClipboardContent{Files: files}, nil
}
