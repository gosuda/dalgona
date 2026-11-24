//go:build gui

package transcript

import (
	"errors"
	"sort"

	"gioui.org/f32"
	"gioui.org/io/pointer"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
)

type linkResult struct {
	target string
	err    error
}

var errNoOpener = errors.New("this window cannot open links; copy the address instead")

func runAt(p document.Projection, offset int) (document.Run, bool) {
	i := sort.Search(len(p.Runs), func(i int) bool { return p.Runs[i].Text.End > offset })
	if i == len(p.Runs) || p.Runs[i].Text.Start > offset {
		return document.Run{}, false
	}
	return p.Runs[i], true
}

func (p *pane) linkAt(at f32.Point) (document.Link, bool) {
	pl, local, ok := p.placedAt(at)
	if !ok || pl.layout == nil {
		return document.Link{}, false
	}
	offset, affinity := pl.layout.Hit(local)
	b := pl.row.doc.Block(pl.block)
	l, span, ok := linkFor(b, offset, affinity)
	return l, ok && linkHit(pl.layout, span, local)
}

func (t *Transcript) openCaretLink(p *pane) {
	if !p.sel.Selected || !p.sel.Selection.Collapsed() {
		return
	}
	caret := p.sel.Selection.Focus
	s, ok := p.resolve(caret)
	if !ok {
		return
	}
	l, _, ok := linkFor(p.order[s.entry].doc.Block(s.block), s.text, caret.Affinity)
	if ok {
		t.openLink(l)
	}
}

func linkFor(b document.Block, offset int, affinity document.Affinity) (document.Link, document.Span, bool) {
	if offset == 0 && affinity == document.Before {
		return document.Link{}, document.Span{}, false
	}
	if affinity == document.Before && offset > 0 {
		offset--
	}
	if offset >= len(b.Projection.Text) {
		return document.Link{}, document.Span{}, false
	}
	run, ok := runAt(b.Projection, offset)
	if !ok || run.Link <= 0 || run.Link > len(b.Links) {
		return document.Link{}, document.Span{}, false
	}
	return b.Links[run.Link-1], run.Text, true
}

func linkHit(layout *document.RowLayout, span document.Span, at f32.Point) bool {
	x := at.X - layout.Inset
	for _, r := range layout.Highlight(span) {
		if x >= r.X0 && x < r.X1 && at.Y >= r.Y0 && at.Y < r.Y1 {
			return true
		}
	}
	return false
}

func (t *Transcript) release(p *pane, e pointer.Event) {
	linked := p.press.linked
	p.press.drag, p.press.linked = false, false
	moved := e.Position.Sub(p.press.where)
	if !linked || moved.X*moved.X+moved.Y*moved.Y >= 64 {
		return
	}
	t.openLink(p.press.link)
}

func (t *Transcript) openLink(l document.Link) {
	target, err := l.Target()
	if err == nil && t.deps.OpenLink == nil {
		err = errNoOpener
	}
	if err != nil {
		t.notice = "The link did not open: " + err.Error()
		return
	}
	t.linkMu.Lock()
	t.linkGen++
	gen := t.linkGen
	open := t.deps.OpenLink
	wake := t.deps.Wake
	t.linkMu.Unlock()
	go func() {
		err := open(target)
		t.linkMu.Lock()
		if gen == t.linkGen {
			t.linkPending = &linkResult{target: target, err: err}
		}
		t.linkMu.Unlock()
		if wake != nil {
			wake()
		}
	}()
}

func (t *Transcript) drainLinks() {
	t.linkMu.Lock()
	pending := t.linkPending
	t.linkPending = nil
	t.linkMu.Unlock()
	if pending == nil {
		return
	}
	if pending.err != nil {
		t.notice = "The link did not open: " + pending.err.Error()
		return
	}
	t.notice = "Opened " + pending.target
}
