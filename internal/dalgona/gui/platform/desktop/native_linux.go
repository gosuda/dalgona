//go:build gui && linux

package desktop

import (
	"context"
	"errors"
	"fmt"
	"image"
	"math"
	"os"
	"sync"

	"gioui.org/app"
	"github.com/godbus/dbus/v5"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type native struct {
	tree       *platform.AccessibilityTree
	mu         sync.Mutex
	parent     string
	reader     platform.ClipboardReader
	origin     image.Point
	originGen  platform.Generation
	originBusy bool
}

func newNative(_ *app.Window, tree *platform.AccessibilityTree) *native {
	return &native{tree: tree}
}

func (n *native) attach(l *platform.Lifetime, g platform.Generation, e app.ViewEvent) error {
	parent, origin := "", func() image.Point { return image.Point{} }
	if x11, ok := e.(app.X11ViewEvent); ok {
		window := uint64(x11.Window)
		if window > math.MaxUint32 {
			return fmt.Errorf("unexpected X11 window %#x: window identifiers fit in 32 bits", x11.Window)
		}
		parent, origin = fmt.Sprintf("x11:%x", x11.Window), n.windowOrigin(uint32(window), g)
	}
	n.mu.Lock()
	n.parent = parent
	n.mu.Unlock()
	clipboardErr := n.openClipboard(l, g, e)
	detach, err := platform.AttachATSPI(n.tree, "dalgona", origin)
	if err != nil {
		return errors.Join(clipboardErr, fmt.Errorf("attach the AT-SPI accessibility bridge: %w", err))
	}
	if !l.Attach(g, detach) {
		detach()
	}
	return clipboardErr
}

func (n *native) openClipboard(l *platform.Lifetime, g platform.Generation, e app.ViewEvent) error {
	display := os.Getenv("DISPLAY")
	if _, ok := e.(app.WaylandViewEvent); ok {
		wayland, err := platform.OpenWaylandClipboard()
		switch {
		case err == nil:
			n.hold(l, g, wayland, wayland.CloseImageClipboard)
			return nil
		case display == "":
			return fmt.Errorf("open the Wayland clipboard: %w", err)
		}
	}
	if display == "" {
		return nil
	}
	x11, err := platform.OpenX11Clipboard(display)
	if err != nil {
		return fmt.Errorf("open the X11 clipboard on %s: %w", display, err)
	}
	n.hold(l, g, x11, func() { _ = x11.Close() })
	return nil
}

func (n *native) hold(l *platform.Lifetime, g platform.Generation, reader platform.ClipboardReader, release func()) {
	n.mu.Lock()
	n.reader = reader
	n.mu.Unlock()
	drop := func() {
		n.mu.Lock()
		if n.reader == reader {
			n.reader = nil
		}
		n.mu.Unlock()
		release()
	}
	if !l.Attach(g, drop) {
		drop()
	}
}

func (n *native) windowOrigin(window uint32, g platform.Generation) func() image.Point {
	return func() image.Point {
		n.mu.Lock()
		if n.originGen != g {
			n.originGen = g
			n.origin = image.Point{}
		}
		if !n.originBusy {
			n.originBusy = true
			go n.fetchOrigin(window, g)
		}
		origin := n.origin
		n.mu.Unlock()
		return origin
	}
}

func (n *native) fetchOrigin(window uint32, g platform.Generation) {
	n.mu.Lock()
	reader, ok := n.reader.(*platform.X11Clipboard)
	n.mu.Unlock()
	if ok {
		if at, err := reader.WindowOrigin(window); err == nil {
			n.mu.Lock()
			if n.originGen == g {
				n.origin = at
			}
			n.mu.Unlock()
		}
	}
	n.mu.Lock()
	n.originBusy = false
	n.mu.Unlock()
}

func (n *native) clipboard() platform.ClipboardReader {
	n.mu.Lock()
	defer n.mu.Unlock()
	return n.reader
}

func (n *native) imageClipboard(ctx context.Context) (clipboardimage.Offer, error) {
	switch reader := n.clipboard().(type) {
	case *platform.X11Clipboard:
		return reader.ImageClipboard(ctx), nil
	case *platform.WaylandClipboard:
		return reader.ImageClipboard(ctx), nil
	default:
		return nil, errors.New("the native image clipboard is unavailable for this window")
	}
}

func (n *native) dialog(ctx context.Context, req platform.DialogRequest) (platform.DialogResult, error) {
	conn, err := dbus.ConnectSessionBus()
	if err != nil {
		return platform.DialogResult{}, fmt.Errorf("%w: %w", platform.ErrPortalUnavailable, err)
	}
	defer conn.Close()
	n.mu.Lock()
	parent := n.parent
	n.mu.Unlock()
	return platform.PortalDialog(ctx, conn, parent, req)
}
