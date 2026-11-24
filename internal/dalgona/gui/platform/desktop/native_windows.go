//go:build gui && windows

package desktop

import (
	"context"
	"fmt"
	"sync/atomic"

	"gioui.org/app"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type native struct {
	window *app.Window
	tree   *platform.AccessibilityTree
	reader platform.ClipboardReader
	owner  atomic.Uintptr
}

func newNative(w *app.Window, tree *platform.AccessibilityTree) *native {
	return &native{window: w, tree: tree, reader: platform.NewWindowsClipboard()}
}

func (n *native) attach(l *platform.Lifetime, g platform.Generation, e app.ViewEvent) error {
	view, ok := e.(app.Win32ViewEvent)
	if !ok {
		return nil
	}
	n.own(l, g, view.HWND)
	var detach func()
	var err error
	n.window.Run(func() { detach, err = platform.AttachUIA(view.HWND, n.tree) })
	if err != nil {
		return fmt.Errorf("attach the UI Automation bridge: %w", err)
	}
	if detach == nil {
		return nil
	}
	if !l.Attach(g, detach) {
		detach()
	}
	return nil
}

func (n *native) own(l *platform.Lifetime, g platform.Generation, hwnd uintptr) {
	n.owner.Store(hwnd)
	release := func() { n.owner.CompareAndSwap(hwnd, 0) }
	if !l.Attach(g, release) {
		release()
	}
}

func (n *native) clipboard() platform.ClipboardReader { return n.reader }

func (n *native) imageClipboard(ctx context.Context) (clipboardimage.Offer, error) {
	owner := n.owner.Load()
	if owner == 0 {
		return nil, ErrNoView
	}
	return platform.NewWindowsClipboardOffer(ctx, owner), nil
}

func (n *native) dialog(ctx context.Context, req platform.DialogRequest) (platform.DialogResult, error) {
	owner := n.owner.Load()
	if owner == 0 {
		return platform.DialogResult{}, ErrNoView
	}
	return showFileDialog(ctx, owner, req)
}
