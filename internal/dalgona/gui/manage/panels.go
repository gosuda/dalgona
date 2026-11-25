//go:build gui

package manage

import (
	"slices"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/panel"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

type Panels struct {
	Rename      *Rename
	Archive     *Archive
	Archived    *Archived
	Endpoints   *Endpoints
	Runner      *Runner
	Servers     *Servers
	Files       *Files
	Terminal    *Terminal
	Diagnostics *Diagnostics
	Failures    *Failures
}

func New(h *Hub) *Panels {
	return &Panels{
		Rename:      newRename(h),
		Archive:     newArchive(h),
		Archived:    newArchived(h),
		Endpoints:   newEndpoints(h),
		Runner:      newRunner(h),
		Servers:     newServers(h),
		Files:       newFiles(h),
		Terminal:    newTerminal(h),
		Diagnostics: newDiagnostics(h),
		Failures:    newFailures(h),
	}
}

type base struct {
	hub     *Hub
	work    tasks
	frame   panel.Frame
	notes   []platform.Announcement
	finish  bool
	escaped bool
}

type scene struct {
	shell.Env
	owner *base
}

type page struct {
	title   string
	body    []layout.Widget
	footer  []action
	dismiss func()
}

func (b *base) init(h *Hub) {
	b.hub = h
	b.work.wake = h.deps.Invalidate
}

func (b *base) reopen() {
	b.frame.Reset()
	b.escaped = false
}

func (b *base) scene(env shell.Env) scene {
	return scene{Env: env, owner: b}
}

func (b *base) show(gtx layout.Context, env scene, p page) layout.Dimensions {
	dismiss := p.dismiss
	if dismiss == nil {
		dismiss = env.Close
	}
	body := slices.DeleteFunc(p.body, func(w layout.Widget) bool { return w == nil })
	actions := make([]layout.Widget, 0, len(p.footer))
	for _, a := range p.footer {
		if a.click != nil {
			actions = append(actions, func(gtx layout.Context) layout.Dimensions { return button(gtx, env, a) })
		}
	}
	dims := b.frame.Layout(gtx, env.Env, panel.Content{
		Title:     p.title,
		Rows:      len(body),
		Row:       func(gtx layout.Context, i int) layout.Dimensions { return body[i](gtx) },
		Actions:   actions,
		OnDismiss: dismiss,
	})
	if b.escaped {
		b.escaped = false
		dismiss()
	}
	return dims
}

func (b *base) announce(kind platform.AnnouncementKind, text string) {
	b.notes = append(b.notes, platform.Announcement{Kind: kind, Text: text})
}

func (b *base) flush(env scene) {
	for _, n := range b.notes {
		if env.Announce != nil {
			env.Announce(n)
		}
	}
	b.notes = b.notes[:0]
	if b.finish {
		b.finish = false
		env.Close()
	}
}
