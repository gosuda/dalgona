//go:build gui && (!darwin || cgo)

package desktop

import (
	"context"
	"errors"
	"sync"
	"sync/atomic"

	"gioui.org/app"
	"gioui.org/io/event"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

var (
	ErrDialogOpen = errors.New("a file dialog is already open; finish or cancel it first")
	ErrNoView     = errors.New("the window has no native view yet; try again after it appears")
)

type DialogOutcome struct {
	Window  platform.Generation
	Request platform.DialogRequest
	Result  platform.DialogResult
	Err     error
}

type Window struct {
	window     *app.Window
	native     *native
	lifetime   platform.Lifetime
	mu         sync.Mutex
	generation platform.Generation
	pending    context.Context
	dialogOpen atomic.Bool
	outcomes   chan DialogOutcome
}

func New(w *app.Window, tree *platform.AccessibilityTree) *Window {
	return &Window{window: w, native: newNative(w, tree), outcomes: make(chan DialogOutcome, 1)}
}

func (d *Window) HandleEvent(ctx context.Context, e event.Event) error {
	switch e := e.(type) {
	case app.ViewEvent:
		if !e.Valid() {
			d.lifetime.End()
			return nil
		}
		generation, pending := d.lifetime.Begin(ctx)
		d.mu.Lock()
		d.generation, d.pending = generation, pending
		d.mu.Unlock()
		return d.native.attach(&d.lifetime, generation, e)
	case app.DestroyEvent:
		d.lifetime.End()
	}
	return nil
}

func (d *Window) snapshot() (platform.Generation, context.Context) {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.generation, d.pending
}

func (d *Window) Clipboard() platform.ClipboardReader {
	return d.native.clipboard()
}

func (d *Window) ImageClipboard() (clipboardimage.Offer, error) {
	g, ctx := d.snapshot()
	if !d.lifetime.Current(g) {
		return nil, ErrNoView
	}
	return d.native.imageClipboard(ctx)
}

func (d *Window) DialogOutcomes() <-chan DialogOutcome { return d.outcomes }

func (d *Window) ShowDialog(req platform.DialogRequest) error {
	if err := req.Check(); err != nil {
		return err
	}
	g, ctx := d.snapshot()
	if !d.lifetime.Current(g) {
		return ErrNoView
	}
	if !d.dialogOpen.CompareAndSwap(false, true) {
		return ErrDialogOpen
	}
	go func() {
		defer d.dialogOpen.Store(false)
		result, err := d.native.dialog(ctx, req)
		if !d.lifetime.Current(g) {
			return
		}
		d.outcomes <- DialogOutcome{Window: g, Request: req, Result: result, Err: err}
		d.window.Invalidate()
	}()
	return nil
}
