//go:build gui && darwin && cgo

package desktop

import (
	"context"
	"fmt"
	"sync"

	"gioui.org/app"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type native struct {
	tree   *platform.AccessibilityTree
	reader platform.ClipboardReader
	mu     sync.Mutex
	parent sheetParent
}

func newNative(_ *app.Window, tree *platform.AccessibilityTree) *native {
	return &native{tree: tree, reader: platform.NewAppKitClipboard()}
}

func (n *native) attach(l *platform.Lifetime, g platform.Generation, e app.ViewEvent) error {
	view, ok := e.(app.AppKitViewEvent)
	if !ok {
		return nil
	}
	n.own(l, g, view.View)
	detach, err := platform.AttachAppKitAccessibility(view.View, n.tree)
	if err != nil {
		return fmt.Errorf("attach the AppKit accessibility bridge: %w", err)
	}
	if !l.Attach(g, detach) {
		detach()
	}
	return nil
}

func (n *native) own(l *platform.Lifetime, g platform.Generation, view uintptr) {
	parent := newSheetParent(view)
	n.mu.Lock()
	n.parent = parent
	n.mu.Unlock()
	release := func() {
		n.mu.Lock()
		if n.parent == parent {
			n.parent = sheetParent{}
		}
		n.mu.Unlock()
		parent.release()
	}
	if !l.Attach(g, release) {
		release()
	}
}

func (n *native) clipboard() platform.ClipboardReader { return n.reader }

func (n *native) imageClipboard(ctx context.Context) (clipboardimage.Offer, error) {
	return platform.NewAppKitImageClipboardOffer(ctx), nil
}

func (n *native) dialog(ctx context.Context, req platform.DialogRequest) (platform.DialogResult, error) {
	n.mu.Lock()
	s, err := beginSheet(n.parent, req)
	n.mu.Unlock()
	if err != nil {
		return platform.DialogResult{}, err
	}
	return s.await(ctx)
}
