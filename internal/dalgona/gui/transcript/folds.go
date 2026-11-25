//go:build gui

package transcript

import (
	"maps"
	"slices"

	"gioui.org/op"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
)

var foldWords = map[document.FoldKind]string{document.FoldCode: "code", document.FoldDiff: "file", document.FoldQuote: "quotation"}

type foldCache struct {
	doc    *document.Document
	folds  []document.Fold
	clicks map[document.Key]*widget.Clickable
}

func (c *foldCache) list(doc *document.Document) []document.Fold {
	if c.doc == doc || doc == nil {
		return c.folds
	}
	c.doc, c.folds = doc, doc.Folds()
	maps.DeleteFunc(c.clicks, func(key document.Key, _ *widget.Clickable) bool {
		return !slices.ContainsFunc(c.folds, func(f document.Fold) bool { return f.Key == key })
	})
	return c.folds
}

func (c *foldCache) click(key document.Key) *widget.Clickable {
	if c.clicks == nil {
		c.clicks = map[document.Key]*widget.Clickable{}
	}
	click, ok := c.clicks[key]
	if !ok {
		click = new(widget.Clickable)
		c.clicks[key] = click
	}
	return click
}

func (r *row) foldFor(ref document.RowRef, scope *document.Scope) (document.Fold, bool) {
	if ref.Row != 0 || ref.Block < 0 {
		return document.Fold{}, false
	}
	for _, f := range r.folds.list(r.doc) {
		if f.Blocks.Start == ref.Block && (ref.Hidden == 0 || scope.Folded(f.Key)) {
			return f, true
		}
	}
	return document.Fold{}, false
}

func (r *row) foldAction(ref document.RowRef, scope *document.Scope) (rowAction, bool) {
	fold, ok := r.foldFor(ref, scope)
	if !ok {
		return rowAction{}, false
	}
	verb := "Collapse "
	if ref.Hidden > 0 {
		verb = "Expand "
	}
	return rowAction{click: r.folds.click(fold.Key), name: verb + foldWords[fold.Kind], act: func(f *frame) {
		f.pane.folding = append(f.pane.folding, fold)
		f.gtx.Execute(op.InvalidateCmd{})
	}}, true
}

func (p *pane) applyFolds() bool {
	toggled := len(p.folding) > 0
	for _, fold := range p.folding {
		p.sel.ToggleFold(p.vp, fold)
	}
	p.folding = p.folding[:0]
	return toggled
}

func carryFolds(from *document.Scope, to *document.State, v *document.Viewport, rows []*row) {
	for _, r := range rows {
		for _, f := range r.folds.list(r.doc) {
			if from.Folded(f.Key) {
				to.ToggleFold(v, f)
			}
		}
	}
}
