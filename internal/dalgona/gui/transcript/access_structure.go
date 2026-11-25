//go:build gui

package transcript

import (
	"cmp"
	"fmt"
	"path"
	"sort"
	"strconv"
	"strings"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	gmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func (t *Transcript) registerSpecial(gtx layout.Context, b document.Block, n platform.SemanticNode, a access.Actions) {
	if n.Role == platform.RoleMath {
		t.registerFormula(gtx, b, n, a)
		return
	}
	s := t.figures[b.Key]
	n.Name = b.Language + " figure"
	caption := strings.TrimSpace(b.Projection.Text[:b.Body.Start])
	if s != nil && s.canvas != nil {
		n.Name = cmp.Or(s.canvas.List.Title, n.Name)
	}
	access.Offscreen(gtx, n, a)
	registerSemanticText(gtx, n.ID+"/caption", n.ID, "Caption", caption)
	if s == nil || s.canvas == nil {
		return
	}
	for i, label := range s.canvas.List.Labels {
		id := n.ID + "/label/" + platform.NodeID(strconv.Itoa(i))
		registerSemanticText(gtx, id, n.ID, "", label)
	}
	name := "Show source"
	if s.source {
		name = "Show figure"
	}
	button := platform.SemanticNode{ID: n.ID + "/source-toggle", Role: platform.RoleButton, Name: name, Action: "press"}
	access.Offscreen(gtx, button, access.Within(n.ID, access.Actions{Tag: &s.toggle, Scroll: a.Scroll, Activate: func() { s.source = !s.source }}))
}

func (t *Transcript) registerFormula(gtx layout.Context, b document.Block, n platform.SemanticNode, a access.Actions) {
	n.Name = "Formula"
	if !t.mathReady() {
		access.Offscreen(gtx, n, a)
		return
	}
	s := t.figureState(b)
	if s.formula == nil && s.err == nil {
		s.formula, s.err = t.mathLayout(b.Projection.Text, displaySize(gtx), true)
	}
	if s.err != nil {
		n.Description = s.err.Error()
	}
	t.expression(gtx, n, a, s.formula)
}

func (t *Transcript) registerInlineMath(gtx layout.Context, b document.Block, ref document.RowRef, id, parent platform.NodeID) {
	if b.Kind == document.KindMath || !t.mathReady() {
		return
	}
	span, runs := b.Rows[ref.Row], b.Projection.Runs
	at := sort.Search(len(runs), func(i int) bool { return runs[i].Text.Start >= span.Start })
	for _, run := range runs[at:] {
		if run.Text.Start >= span.End {
			break
		}
		if run.Kind != document.Formula {
			continue
		}
		l, ok := t.inlineFormula(gtx, b, run)
		if !ok {
			continue
		}
		key := id + "/math/" + platform.NodeID(strconv.Itoa(run.Text.Start))
		n := platform.SemanticNode{ID: key, Role: platform.RoleMath, Name: "Inline formula", Value: b.Projection.Text[run.Text.Start:run.Text.End]}
		t.expression(gtx, n, access.Within(parent, access.Actions{}), l)
	}
}

func (t *Transcript) expression(gtx layout.Context, root platform.SemanticNode, a access.Actions, l *gmath.Layout) {
	if l == nil {
		access.Offscreen(gtx, root, a)
		return
	}
	expr := l.Expression()
	root.Description = boundedName(expr[0].Label)
	access.Offscreen(gtx, root, a)
	for i := 1; i < len(expr); i++ {
		part := expr[i]
		parent := root.ID
		if part.Parent > 0 {
			parent = expressionID(root.ID, part.Parent)
		}
		n := platform.SemanticNode{ID: expressionID(root.ID, i), Role: platform.RoleMath, Name: boundedName(part.Label), Description: part.Role, Value: part.Source}
		access.Offscreen(gtx, n, access.Within(parent, access.Actions{Scroll: a.Scroll}))
	}
}

func expressionID(root platform.NodeID, i int) platform.NodeID {
	return root + "/expression/" + platform.NodeID(strconv.Itoa(i))
}

func registerSemanticText(gtx layout.Context, id, parent platform.NodeID, name, text string) {
	for i, span := range platform.TextObjects(text) {
		n := platform.SemanticNode{ID: id + "/" + platform.NodeID(strconv.Itoa(i)), Role: platform.RoleDocument, Name: name, Value: text[span.Start:span.End]}
		access.Offscreen(gtx, n, access.Within(parent, access.Actions{}))
	}
}

func (meta attachment) name() string {
	base := path.Base(strings.ReplaceAll(meta.Path, "\\", "/"))
	if base == "." || base == "/" {
		base = ""
	}
	return cmp.Or(meta.Name, base, "Image "+strconv.Itoa(meta.Index+1))
}

func (t *Transcript) registerImages(gtx layout.Context, p *pane, index int, r *row) {
	if len(r.sheet.images) == 0 {
		return
	}
	for i, img := range t.imagesOf(r) {
		meta := r.sheet.images[i]
		id := platform.NodeID(access.Path(r.id.Turn, r.id.Item, r.id.Draft, "image", strconv.Itoa(i)))
		n := platform.SemanticNode{ID: id, Role: platform.RoleImage, Name: meta.name(), Description: meta.Alt}
		scroll := func() { p.scrolled = p.vp.RowTop(index) - p.vp.Offset() }
		access.Offscreen(gtx, n, access.Actions{Scroll: scroll})
		preview := platform.SemanticNode{ID: id + "/preview", Role: platform.RoleButton, Name: previewAction(meta, img.hidden), Action: "press", Disabled: t.deps.Figures == nil}
		a := access.Actions{Tag: &img.toggle, Scroll: scroll, Activate: func() { img.hidden = !img.hidden; scroll() }}
		access.Offscreen(gtx, preview, access.Within(id, a))
		if img.err != nil {
			notice := platform.SemanticNode{ID: id + "/error", Role: platform.RoleError, Name: fmt.Sprintf("The preview for %s could not load: %v", n.Name, img.err), Target: access.ID(gtx, string(preview.ID))}
			access.Offscreen(gtx, notice, access.Within(id, access.Actions{}))
		}
	}
}
