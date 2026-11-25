//go:build gui

package panel

import (
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type Dialogs interface {
	ShowDialog(req platform.DialogRequest) error
}

type panel interface {
	open(r shell.Route)
	layout(gtx layout.Context, s *scene) layout.Dimensions
	release(out *[]state.Intent)
}

var refusedWhileRunning = map[string]bool{"new": true, "compact": true, "fork": true, "tree": true, "revert": true}

type Panels struct {
	frame    Frame
	route    shell.Route
	current  panel
	guard    refusal
	checked  bool
	out      []state.Intent
	help     keyPanel
	model    modelPanel
	settings settingsPanel
	login    loginPanel
	resume   resumePanel
	start    newThreadPanel
	compact  compactPanel
	fork     forkPanel
	tree     treePanel
	rewind   rewindPanel
	advisor  advisorPanel
	review   reviewPanel
}

func New(notifications shell.Widget, dialogs Dialogs) *Panels {
	p := &Panels{}
	p.settings.notifications = notifications
	p.start.dialogs = dialogs
	return p
}

func (p *Panels) lookup(name string) panel {
	routes := map[string]panel{
		"help": &p.help, "model": &p.model, "settings": &p.settings, "login": &p.login, "resume": &p.resume,
		"new": &p.start, "compact": &p.compact, "fork": &p.fork, "tree": &p.tree, "revert": &p.rewind,
		"advisor": &p.advisor, "review": &p.review,
	}
	return routes[name]
}

func (p *Panels) Opened(r shell.Route) {
	if p.current != nil {
		p.current.release(&p.out)
	}
	p.route = r
	p.current = p.lookup(r.Panel)
	p.frame.Reset()
	p.guard = refusal{}
	p.checked = false
	if p.current != nil {
		p.current.open(r)
	}
}

func (p *Panels) FolderChosen(result platform.DialogResult, err error) {
	p.start.chosen(result, err)
}

func (p *Panels) Layout(gtx layout.Context, env shell.Env) layout.Dimensions {
	s := &scene{env: env, frame: &p.frame, out: &p.out}
	s.close = func() { p.close(s) }
	p.flush(env)
	if p.current == nil {
		return layout.Dimensions{}
	}
	if !p.checked {
		p.checked = true
		p.guard = refuse(env, p.route)
	}
	if p.guard.turn != "" {
		return p.guard.layout(gtx, s)
	}
	dims := p.current.layout(gtx, s)
	p.flush(env)
	return dims
}

func (p *Panels) flush(env shell.Env) {
	for _, i := range p.out {
		env.Submit(i)
	}
	p.out = p.out[:0]
}

func (p *Panels) close(s *scene) {
	if p.current != nil {
		p.current.release(&p.out)
	}
	p.current = nil
	p.flush(s.env)
	s.env.Close()
}

func (p *Panels) Settle(env shell.Env) {
	s := &scene{env: env, frame: &p.frame, out: &p.out}
	s.close = func() { p.close(s) }
	p.login.settle(s)
	p.flush(env)
}
