//go:build gui

package main

import (
	"context"
	"errors"
	"fmt"
	"image"
	"log/slog"
	"sync"

	"gioui.org/app"
	"gioui.org/io/event"
	"gioui.org/io/system"
	"gioui.org/op"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/composer"
	"github.com/gosuda/dalgona/internal/dalgona/gui/decision"
	gmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
	"github.com/gosuda/dalgona/internal/dalgona/gui/manage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/panel"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform/desktop"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
	"github.com/gosuda/dalgona/internal/dalgona/gui/transcript"
)

const (
	appTitle        = "dalgona"
	intentQueue     = 64
	activationQueue = 4
)

var errWindowClosed = errors.New("the window closed before the dialog finished; open the dialog again")

type window struct {
	app         *application
	id          state.WindowID
	w           *app.Window
	ctx         context.Context
	cancel      context.CancelFunc
	native      *desktop.Window
	tree        *platform.AccessibilityTree
	semantics   access.Frame
	frame       *shell.Shell
	composer    *composer.Dock
	transcript  *transcript.Transcript
	panels      *panel.Panels
	terminal    windowTerminals
	dialogs     *dialogRouter
	activations chan state.ThreadKey
	title       string
	focused     bool
	ops         op.Ops
}

type windowTerminals interface {
	AttachedSession() (terminalID string, thread state.ThreadKey, ok bool)
	CloseSessions()
}

func newWindow(a *application, id state.WindowID) (*window, error) {
	w := new(app.Window)
	ctx, cancel := context.WithCancel(a.ctx)
	win := &window{app: a, id: id, w: w, ctx: ctx, cancel: cancel, activations: make(chan state.ThreadKey, activationQueue), title: appTitle}
	win.tree = platform.NewAccessibilityTree(intentQueue, w.Invalidate)
	win.native = desktop.New(w, win.tree)
	win.dialogs = &dialogRouter{native: win.native, done: ctx.Done()}
	if err := win.build(); err != nil {
		cancel()
		return nil, err
	}
	w.Option(
		app.Title(appTitle),
		app.Size(unit.Dp(theme.WindowDefaultWidth), unit.Dp(theme.WindowDefaultHeight)),
		app.MinSize(unit.Dp(theme.WindowMinWidth), unit.Dp(theme.WindowMinHeight)),
	)
	return win, nil
}

func (win *window) build() error {
	shaper, err := platform.NewShaper()
	if err != nil {
		return fmt.Errorf("load bundled fonts: %w", err)
	}
	mathFont, mathText, err := loadMath()
	if err != nil {
		return err
	}
	dock, err := composer.New(composer.Host{PickImages: win.dialogs.images, Wake: win.w.Invalidate, Figures: win.app.figures})
	if err != nil {
		return fmt.Errorf("create the composer: %w", err)
	}
	win.composer = dock
	win.panels = panel.New(win.app.attention.Row(), win.dialogs)
	views := transcript.New(transcript.Deps{
		Figures: win.app.figures, Math: mathFont, MathText: mathText,
		Save: win.dialogs.choose, OpenLink: win.openLink, Wake: win.w.Invalidate, CopyImage: win.native.ImageClipboard,
	})
	win.transcript = views
	managed := manage.New(win.app.hub)
	win.terminal = managed.Terminal
	win.frame = shell.New(shell.Config{
		Shaper: shaper, Window: win.id, Submit: win.app.owner.Submit, Announce: win.tree.Announce,
		Clipboard: win.native.Clipboard, Wake: win.w.Invalidate, NewWindow: win.app.newWindow, CloseWindow: win.requestClose,
		Parts: shell.Parts{
			Transcript: views, Composer: dock, Request: decision.NewDock(),
			Panels: panelRoutes(win.panels, managed), Settlers: []shell.Settler{win.panels},
		},
	})
	return nil
}

func loadMath() (*gmath.Font, gmath.TextFace, error) {
	data, err := platform.MathFontData()
	if err != nil {
		return nil, nil, fmt.Errorf("load the bundled math font: %w", err)
	}
	font, err := gmath.ParseFont(data)
	if err != nil {
		return nil, nil, fmt.Errorf("parse the bundled math font: %w", err)
	}
	text, err := gmath.NewTextFont(platform.MathTextFaceData()...)
	if err != nil {
		return nil, nil, fmt.Errorf("parse the bundled math text faces: %w", err)
	}
	return font, text, nil
}

func panelRoutes(p *panel.Panels, m *manage.Panels) map[string]shell.Panel {
	routes := map[string]shell.Panel{
		"remote": m.Endpoints, "runner": m.Runner, "mcp": m.Servers, "files": m.Files, "terminal": m.Terminal,
		"doctor": m.Diagnostics, "failures": m.Failures, "rename": m.Rename, "archive": m.Archive, "archived": m.Archived,
	}
	for _, name := range []string{"help", "model", "settings", "login", "resume", "new", "compact", "fork", "tree", "revert", "advisor", "review"} {
		routes[name] = p
	}
	return routes
}

func (win *window) openLink(u string) error { return platform.OpenLink(win.ctx, u) }

func (win *window) loop() error {
	defer win.closeSessions()
	defer win.composer.Close()
	defer win.transcript.Close()
	for {
		if done, err := win.handle(win.w.Event()); done {
			return err
		}
	}
}

func (win *window) closeSessions() {
	if win.terminal != nil {
		win.terminal.CloseSessions()
	}
}

func (win *window) handle(e event.Event) (bool, error) {
	if err := win.native.HandleEvent(win.ctx, e); err != nil {
		slog.Warn("desktop integration", "window", win.id, "error", err)
	}
	switch e := e.(type) {
	case app.DestroyEvent:
		return true, e.Err
	case app.ViewEvent:
		win.dialogs.deliver(desktop.DialogOutcome{Err: desktop.ErrNoView}, win.panels)
	case app.ConfigEvent:
		win.configure(e.Config.Focused)
	case *app.ClosingEvent:
		if win.closing() {
			e.Abort()
			win.w.Invalidate()
		}
	case app.FrameEvent:
		win.draw(e)
	}
	return false, nil
}

func (win *window) configure(focused bool) {
	if focused && !win.focused {
		win.app.appearanceChanged()
		win.app.attention.Refresh()
	}
	win.focused = focused
	win.app.observe(win)
}

func (win *window) closing() bool {
	return win.frame.Closing(win.app.view(), win.app.closeScope(win.id))
}

func (win *window) requestClose() {
	if win.closing() {
		win.w.Invalidate()
		return
	}
	win.w.Perform(system.ActionClose)
}

func (win *window) activate(key state.ThreadKey) {
	select {
	case win.activations <- key:
	default:
	}
	win.w.Invalidate()
}

func (win *window) drainActivations() {
	for {
		select {
		case key := <-win.activations:
			win.frame.Show(key)
			win.w.Perform(system.ActionRaise)
		default:
			return
		}
	}
}

func (win *window) draw(e app.FrameEvent) {
	win.drainActivations()
	win.dialogs.drain(win.panels)
	v := win.app.view()
	l := win.app.look()
	fr := shell.Frame{Theme: win.app.themeFor(v, win, l), View: v, Still: l.still}
	gtx := app.NewContext(&win.ops, e)
	gtx = win.semantics.Begin(gtx, win.tree, image.Rectangle{Max: e.Size})
	confirmed := win.frame.Layout(gtx, fr)
	e.Frame(gtx.Ops)
	var focused event.Tag
	win.w.Run(func() { focused = win.semantics.Focus(gtx) })
	if err := win.semantics.Publish(gtx, win.tree, focused); err != nil {
		slog.Error("publish window accessibility", "window", win.id, "error", err)
	}
	win.app.observe(win)
	if title := win.frame.WindowTitle(fr); title != win.title {
		win.title = title
		win.w.Option(app.Title(title))
	}
	if confirmed {
		win.w.Perform(system.ActionClose)
	}
}

type dialogOwner uint8

const (
	noDialog dialogOwner = iota
	chooserDialog
	panelDialog
)

type dialogRouter struct {
	native  *desktop.Window
	done    <-chan struct{}
	mu      sync.Mutex
	owner   dialogOwner
	waiting chan desktop.DialogOutcome
}

func (d *dialogRouter) ShowDialog(req platform.DialogRequest) error {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.open(req, panelDialog, nil)
}

func (d *dialogRouter) open(req platform.DialogRequest, owner dialogOwner, wait chan desktop.DialogOutcome) error {
	if d.owner != noDialog {
		return desktop.ErrDialogOpen
	}
	if err := d.native.ShowDialog(req); err != nil {
		return err
	}
	d.owner, d.waiting = owner, wait
	return nil
}

func (d *dialogRouter) choose(ctx context.Context, req platform.DialogRequest) (platform.DialogResult, error) {
	wait := make(chan desktop.DialogOutcome, 1)
	d.mu.Lock()
	err := d.open(req, chooserDialog, wait)
	d.mu.Unlock()
	if err != nil {
		return platform.DialogResult{}, err
	}
	select {
	case <-ctx.Done():
		return platform.DialogResult{}, ctx.Err()
	case <-d.done:
		return platform.DialogResult{}, errWindowClosed
	case out := <-wait:
		return out.Result, out.Err
	}
}

func (d *dialogRouter) images(ctx context.Context) ([]string, error) {
	req := platform.DialogRequest{Kind: platform.DialogOpenFiles, Title: "Attach images", Filters: []platform.FileFilter{{Name: "Images", Extensions: []string{"png", "jpg", "jpeg", "gif"}}}}
	result, err := d.choose(ctx, req)
	return result.Files, err
}

func (d *dialogRouter) drain(panels *panel.Panels) {
	for {
		select {
		case out := <-d.native.DialogOutcomes():
			d.deliver(out, panels)
		default:
			return
		}
	}
}

func (d *dialogRouter) take() (dialogOwner, chan desktop.DialogOutcome) {
	d.mu.Lock()
	defer d.mu.Unlock()
	owner, wait := d.owner, d.waiting
	d.owner, d.waiting = noDialog, nil
	return owner, wait
}

func (d *dialogRouter) deliver(out desktop.DialogOutcome, panels *panel.Panels) {
	switch owner, wait := d.take(); owner {
	case chooserDialog:
		wait <- out
	case panelDialog:
		panels.FolderChosen(out.Result, out.Err)
	case noDialog:
	}
}
