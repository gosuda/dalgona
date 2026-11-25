//go:build gui

package transcript

import (
	"context"
	"errors"
	"image"
	"image/color"
	"strconv"
	"strings"

	"gioui.org/f32"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	gmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const formulaCacheLimit = 1024

const figureStateLimit = 64

type Deps struct {
	Figures   *figure.Hub
	Math      *gmath.Font
	MathText  gmath.TextFace
	MathMono  gmath.TextFace
	Save      func(context.Context, platform.DialogRequest) (platform.DialogResult, error)
	CopyImage func() (clipboardimage.Offer, error)
	OpenLink  func(string) error
	Wake      func()
}

type figureState struct {
	key     string
	job     uint64
	err     error
	canvas  *figure.Canvas
	source  bool
	seen    uint64
	toggle  widget.Clickable
	copy    widget.Clickable
	svg     widget.Clickable
	png     widget.Clickable
	retry   widget.Clickable
	resume  widget.Clickable
	formula *gmath.Layout
}

type formulaKey struct {
	tex  string
	size float32
}

func (t *Transcript) special(f *frame, b document.Block) (layout.Dimensions, bool) {
	switch b.Kind {
	case document.KindFigure:
		return t.figure(f, b)
	case document.KindMath:
		return t.formula(f, b)
	case document.KindParagraph, document.KindHeading, document.KindCode, document.KindTable, document.KindRule, document.KindHTML, document.KindNotice, document.KindDiff:
	}
	return layout.Dimensions{}, false
}

func (t *Transcript) figureState(b document.Block) *figureState {
	if t.figures == nil {
		t.figures = map[document.Key]*figureState{}
	}
	s, ok := t.figures[b.Key]
	if !ok {
		s = &figureState{}
		t.figures[b.Key] = s
	}
	return s
}

func figureTheme(p theme.Palette) figure.Theme {
	c := func(n color.NRGBA) figure.Color { return figure.Color{R: n.R, G: n.G, B: n.B, A: n.A} }
	return figure.Theme{Canvas: c(p.Canvas), Surface: c(p.Surface), Tone: c(p.Tone), Line: c(p.Line), Strong: c(p.Strong), Text: c(p.Text), Dim: c(p.Dim), Accent: c(p.AccentText)}
}

func (t *Transcript) figure(f *frame, b document.Block) (layout.Dimensions, bool) {
	kind, known := figure.ParseKind(strings.ToLower(b.Language))
	if !known || b.Key.Provisional || t.deps.Figures == nil {
		return layout.Dimensions{}, false
	}
	s := t.figureState(b)
	source := b.Projection.Text[b.Body.Start:b.Body.End]
	if f.ref.Row > 0 {
		return layout.Dimensions{}, s.canvas != nil && !s.source
	}
	t.renderFigure(f, s, kind, source, false)
	if s.canvas == nil || s.source {
		t.figureStatus(f, s, kind, source)
		return layout.Dimensions{}, false
	}
	return t.drawFigure(f, s, kind, source), true
}

func (t *Transcript) renderFigure(f *frame, s *figureState, kind figure.Kind, source string, retry bool) {
	hub := t.deps.Figures
	s.seen = t.figureClock
	width := float64(f.column) / float64(f.gtx.Metric.PxPerDp)
	req := figure.Request{
		Document: f.row.id.Item, Revision: uint64(f.row.doc.Revision()), Kind: kind, Source: []byte(source),
		Theme: figureTheme(t.palette), Scale: float64(f.gtx.Metric.PxPerDp), Viewport: [2]float64{width, width}, Visible: f.ref.Row == 0, Retry: retry,
	}
	if key := hub.Supervisor.ContentKey(req); key != s.key {
		hub.Supervisor.Cancel(s.job)
		s.key, s.job, s.canvas, s.err = key, 0, nil, nil
	}
	if s.canvas != nil || s.err != nil {
		return
	}
	if !retry && t.loadFigureOutcome(f, s) {
		return
	}
	if retry && s.job != 0 {
		hub.Supervisor.Cancel(s.job)
		s.job = 0
	}
	id, _, err := hub.Supervisor.Render(req)
	if errors.Is(err, figure.ErrWaiting) {
		return
	}
	s.job, s.err = id, err
}

func (t *Transcript) loadFigureOutcome(f *frame, s *figureState) bool {
	if o, ok := t.deps.Figures.Result(s.key); ok && s.adopts(o) {
		s.accept(o, f)
	}
	return s.canvas != nil || s.err != nil || s.job != 0
}

func (s *figureState) adopts(o figure.Outcome) bool {
	return s.job == 0 || o.Job == s.job
}

func (s *figureState) accept(o figure.Outcome, f *frame) {
	if o.Err != nil {
		s.err = o.Err
		return
	}
	if o.List != nil {
		s.canvas = figure.NewCanvas(o.List, f.env.Shaper, figure.Faces{Sans: platform.ProseFace, Mono: platform.CodeFace})
	}
}

func (t *Transcript) sweepFigures() {
	if t.deps.Figures == nil {
		return
	}
	for _, s := range t.figures {
		if s.seen != t.figureClock && s.job != 0 && s.canvas == nil && s.err == nil {
			t.deps.Figures.Supervisor.Cancel(s.job)
			s.job = 0
		}
	}
	for len(t.figures) > figureStateLimit {
		oldest, s := t.oldestFigure()
		if s.job != 0 {
			t.deps.Figures.Supervisor.Cancel(s.job)
		}
		delete(t.figures, oldest)
	}
}

func (t *Transcript) oldestFigure() (document.Key, *figureState) {
	var key document.Key
	var oldest *figureState
	for k, s := range t.figures {
		if oldest == nil || s.seen < oldest.seen {
			key, oldest = k, s
		}
	}
	return key, oldest
}

func (t *Transcript) figureStatus(f *frame, s *figureState, kind figure.Kind, source string) layout.Dimensions {
	text := "Rendering the " + kind.String() + " figure"
	failed := s.err != nil
	if failed {
		text = "The " + kind.String() + " figure did not render: " + s.err.Error()
	}
	if s.retry.Clicked(f.gtx) {
		s.err, s.canvas = nil, nil
		t.renderFigure(f, s, kind, source, true)
	}
	if s.resume.Clicked(f.gtx) {
		t.deps.Figures.Supervisor.Resume()
		s.err = nil
		t.renderFigure(f, s, kind, source, true)
	}
	return f.stack(func(gtx layout.Context) layout.Dimensions {
		children := []layout.FlexChild{layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
			c := t.palette.Dim
			if failed {
				c = t.palette.FailedText
			}
			return label(gtx, f.env.Shaper, caption(c), text)
		})}
		if s.canvas != nil {
			children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.toggle, "Show figure", func() { s.source = false })
			}))
		}
		if failed {
			children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				if t.deps.Figures.Supervisor.Paused() {
					return t.ctrl.button(gtx, &s.resume, "Resume rendering", false)
				}
				return t.ctrl.button(gtx, &s.retry, "Retry", false)
			}))
		}
		return layout.Flex{Alignment: layout.Middle}.Layout(gtx, children...)
	})
}

func (t *Transcript) figureButton(gtx layout.Context, s *figureState, click *widget.Clickable, name string, act func()) layout.Dimensions {
	if click.Clicked(gtx) {
		act()
	}
	return layout.Inset{Left: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		if click != &s.toggle {
			return t.ctrl.button(gtx, click, name, false)
		}
		dims := t.ctrl.button(access.Hide(gtx), click, name, false)
		access.Place(gtx, image.Rectangle{Max: dims.Size}, click)
		return dims
	})
}

func (t *Transcript) drawFigure(f *frame, s *figureState, kind figure.Kind, source string) layout.Dimensions {
	gtx := f.gtx
	natural := s.canvas.List.Width * gtx.Metric.PxPerDp
	s.canvas.Zoom = 1
	if natural > float32(f.column) && natural > 0 {
		s.canvas.Zoom = float32(f.column) / natural
	}
	stack := op.Offset(image.Pt(0, f.y)).Push(gtx.Ops)
	macro := op.Record(gtx.Ops)
	dims := s.canvas.Layout(gtx)
	call := macro.Stop()
	area := clipArea(gtx, dims.Size)
	semantic.LabelOp(kind.String() + " figure").Add(gtx.Ops)
	semantic.DescriptionOp(firstLine(source)).Add(gtx.Ops)
	call.Add(gtx.Ops)
	area.Pop()
	stack.Pop()
	actions := f.stackAt(dims.Size.Y, func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{}.Layout(gtx,
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.toggle, "Show source", func() { s.source = true })
			}),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.copy, "Copy source", func() { t.copySource(gtx, f, kind.String()+" source") })
			}),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.svg, "Save SVG", func() { t.saveDiagram(s.canvas.List, ".svg", 0) })
			}),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.png, "Save PNG", func() { t.saveDiagram(s.canvas.List, ".png", gtx.Metric.PxPerDp) })
			}),
		)
	})
	return layout.Dimensions{Size: image.Pt(f.column, dims.Size.Y+actions)}
}

func (t *Transcript) copySource(gtx layout.Context, f *frame, what string) {
	if err := f.row.doc.CopySource(gtx.Source, f.ref.Block); err != nil {
		t.notice = "Copy failed: " + err.Error()
		return
	}
	t.notice = "Copied the " + what + "."
}

func (f *frame) stackAt(top int, w layout.Widget) int {
	gtx := f.gtx
	gtx.Constraints = layout.Constraints{Max: image.Pt(f.column, infinite)}
	stack := op.Offset(image.Pt(0, f.y+top)).Push(gtx.Ops)
	dims := w(gtx)
	stack.Pop()
	return dims.Size.Y
}

func firstLine(s string) string {
	line, _, _ := strings.Cut(strings.TrimSpace(s), "\n")
	return line
}

func (t *Transcript) formula(f *frame, b document.Block) (layout.Dimensions, bool) {
	if !t.mathReady() || b.Key.Provisional {
		return layout.Dimensions{}, false
	}
	s := t.figureState(b)
	if f.ref.Row > 0 {
		return layout.Dimensions{}, s.formula != nil
	}
	source := b.Projection.Text
	if s.formula == nil && s.err == nil {
		s.formula, s.err = t.mathLayout(source, displaySize(f.gtx), true)
	}
	if s.err != nil {
		f.stack(func(gtx layout.Context) layout.Dimensions {
			return label(gtx, f.env.Shaper, caption(t.palette.FailedText), "The formula did not render: "+s.err.Error())
		})
		return layout.Dimensions{}, false
	}
	gtx := f.gtx
	size := image.Pt(int(s.formula.Width+0.5), int(s.formula.Ascent+s.formula.Descent+0.5))
	stack := op.Offset(image.Pt(0, f.y)).Push(gtx.Ops)
	area := clipArea(gtx, size)
	semantic.LabelOp("formula: " + source).Add(gtx.Ops)
	s.formula.Draw(gtx.Ops, t.palette.Text)
	area.Pop()
	stack.Pop()
	actions := f.stackAt(size.Y, func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{}.Layout(gtx,
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.copy, "Copy TeX", func() { t.copySource(gtx, f, "TeX source") })
			}),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.svg, "Save SVG", func() { t.saveFormula(source, ".svg", 0) })
			}),
			layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return t.figureButton(gtx, s, &s.png, "Save PNG", func() { t.saveFormula(source, ".png", gtx.Metric.PxPerDp) })
			}),
		)
	})
	return layout.Dimensions{Size: image.Pt(f.column, size.Y+actions)}, true
}

func clipArea(gtx layout.Context, size image.Point) clip.Stack {
	return clip.Rect{Max: size}.Push(gtx.Ops)
}

func (t *Transcript) mathReady() bool {
	return t.deps.Math != nil && t.deps.MathText != nil
}

func displaySize(gtx layout.Context) float32 {
	return float32(gtx.Sp(unit.Sp(theme.SizeBody))) * 1.2
}

func (t *Transcript) mathLayout(source string, size float32, display bool) (*gmath.Layout, error) {
	parsed, err := gmath.Parse(source)
	if err != nil {
		return nil, err
	}
	return t.deps.Math.Layout(parsed, gmath.Options{Size: size, Display: display, Text: t.deps.MathText})
}

func inlineSize(gtx layout.Context, b document.Block) float32 {
	typo := typography(1)
	size := typo.Size
	if b.Kind == document.KindHeading && b.Level >= 1 && b.Level <= len(typo.Headings) {
		size = typo.Headings[b.Level-1]
	}
	return float32(gtx.Sp(size))
}

func (t *Transcript) inlineFormula(gtx layout.Context, b document.Block, run document.Run) (*gmath.Layout, bool) {
	key := formulaKey{tex: b.Projection.Text[run.Text.Start:run.Text.End], size: inlineSize(gtx, b)}
	if l, ok := t.formulas[key]; ok {
		return l, l != nil
	}
	if t.formulas == nil || len(t.formulas) >= formulaCacheLimit {
		t.formulas = map[formulaKey]*gmath.Layout{}
	}
	key.tex = strings.Clone(key.tex)
	l, err := t.mathLayout(key.tex, key.size, false)
	if err != nil {
		l = nil
	}
	t.formulas[key] = l
	return l, l != nil
}

func (t *Transcript) inlineBox(gtx layout.Context) func(document.Block, document.Run) (document.InlineBox, bool) {
	return func(b document.Block, run document.Run) (document.InlineBox, bool) {
		l, ok := t.inlineFormula(gtx, b, run)
		if !ok {
			return document.InlineBox{}, false
		}
		return document.InlineBox{Width: l.Width, Ascent: l.Ascent, Descent: l.Descent}, true
	}
}

func formulaBoxes(inline func(document.Block, document.Run) (document.InlineBox, bool), b document.Block, span document.Span) string {
	var out []byte
	for _, run := range b.Projection.Runs {
		if run.Style&document.InlineMath == 0 || run.Text.End <= span.Start || run.Text.Start >= span.End {
			continue
		}
		box, ok := inline(b, run)
		if !ok {
			out = append(out, "-;"...)
			continue
		}
		for _, v := range [...]float32{box.Width, box.Ascent, box.Descent} {
			out = append(strconv.AppendFloat(out, float64(v), 'g', -1, 32), ',')
		}
		out = append(out, ';')
	}
	return string(out)
}

func (t *Transcript) paintFormulas(gtx layout.Context, b document.Block, rl *document.RowLayout, ink color.NRGBA) {
	for _, object := range rl.Objects() {
		run, ok := runAt(b.Projection, object.Text.Start)
		if !ok {
			continue
		}
		l, ok := t.inlineFormula(gtx, b, run)
		if !ok {
			continue
		}
		stack := op.Affine(f32.AffineId().Offset(f32.Pt(object.Rect.X0, object.Rect.Y0))).Push(gtx.Ops)
		l.Draw(gtx.Ops, ink)
		stack.Pop()
	}
}
