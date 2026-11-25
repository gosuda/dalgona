package figure

import (
	"encoding/base64"
	"errors"
	"fmt"
	"math"
	"strings"

	"github.com/go-text/typesetting/font/opentype"
	"oss.terrastruct.com/d2/d2ast"
	"oss.terrastruct.com/d2/d2graph"
	"oss.terrastruct.com/d2/d2target"
	"oss.terrastruct.com/d2/d2themes"
	"oss.terrastruct.com/d2/lib/geo"
	"oss.terrastruct.com/d2/lib/label"
	d2shape "oss.terrastruct.com/d2/lib/shape"
	"oss.terrastruct.com/d2/lib/svg"
	"oss.terrastruct.com/d2/lib/textmeasure"

	figmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
)

const (
	languageMarkdown = "markdown"
	languageLaTeX    = "latex"
)

type overlay struct {
	svg   []byte
	label string
}

type richLabel struct {
	latex    bool
	source   string
	size     float64
	markdown *markdownLayout
	formula  *figmath.Layout
	width    float64
	height   float64
	at       vec
	color    string
}

type richLabels struct {
	ruler    *textmeasure.Ruler
	spans    map[string]d2ast.Range
	formulas formulaSource
	byID     map[string]*richLabel
	measured []*d2target.MText
}

func newRichLabels(ruler *textmeasure.Ruler, spans map[string]d2ast.Range, formulas formulaSource) *richLabels {
	return &richLabels{ruler: ruler, spans: spans, formulas: formulas, byID: map[string]*richLabel{}}
}

func (r *richLabels) measure(g *d2graph.Graph) error {
	for _, obj := range g.Objects {
		if err := r.measureAttributes(obj.AbsID(), &obj.Attributes, obj.Text); err != nil {
			return err
		}
	}
	for _, edge := range g.Edges {
		if err := r.measureAttributes(edge.AbsID(), &edge.Attributes, edge.Text); err != nil {
			return err
		}
	}
	return nil
}

func (r *richLabels) measureAttributes(id string, attrs *d2graph.Attributes, text func() *d2target.MText) error {
	if attrs.Language != languageMarkdown && attrs.Language != languageLaTeX {
		return nil
	}
	attrs.ApplyTextTransform()
	t := text()
	l, err := r.layout(attrs.Language == languageLaTeX, t)
	if err != nil {
		return err
	}
	attrs.Language = languageMarkdown
	r.byID[id] = l
	r.measured = append(r.measured, &d2target.MText{
		Text: t.Text, FontSize: t.FontSize, IsBold: t.IsBold, IsItalic: t.IsItalic, Language: languageMarkdown,
		Dimensions: *d2target.NewTextDimensions(int(math.Ceil(l.width)), int(math.Ceil(l.height))),
	})
	return nil
}

func (r *richLabels) layout(latex bool, t *d2target.MText) (*richLabel, error) {
	l := &richLabel{latex: latex, source: t.Text, size: float64(t.FontSize)}
	if latex {
		formula, err := layoutFormula(t.Text, l.size, r.formulas)
		if err != nil {
			return nil, failAt(FailureInvalid, d2Span(r.spans[t.Text]), "D2 LaTeX label: %v", err)
		}
		l.formula, l.width, l.height = formula, float64(formula.Width), float64(formula.Ascent+formula.Descent)
		return l, nil
	}
	md, err := layoutMarkdown(t.Text, t.FontSize, r.ruler)
	if err != nil {
		return nil, failAt(FailureInvalid, d2Span(r.spans[t.Text]), "D2 Markdown label: %v", err)
	}
	l.markdown, l.width, l.height = md, md.width, md.height
	return l, nil
}

func layoutFormula(source string, size float64, formulas formulaSource) (*figmath.Layout, error) {
	formula, err := figmath.Parse(source)
	if err != nil {
		return nil, err
	}
	if formulas == nil {
		return nil, errors.New("the figure renderer received no math font")
	}
	faces, err := formulas()
	if err != nil {
		return nil, err
	}
	return faces.font.Layout(formula, figmath.Options{Size: float32(size), Display: true, Text: faces.text})
}

func (r *richLabels) place(diagram *d2target.Diagram, theme d2themes.Theme) {
	for i := range diagram.Shapes {
		s := &diagram.Shapes[i]
		l, ok := r.byID[s.ID]
		if !ok {
			continue
		}
		l.at = shapeLabelOrigin(s)
		l.color = d2themes.ResolveThemeColor(theme, s.Color)
		if l.latex {
			l.color = d2themes.ResolveThemeColor(theme, s.Stroke)
		}
		s.Label, s.Language = "", ""
	}
	for i := range diagram.Connections {
		c := &diagram.Connections[i]
		l, ok := r.byID[c.ID]
		if !ok {
			continue
		}
		at := c.GetLabelTopLeft()
		l.at = vec{math.Round(at.X), math.Round(at.Y)}
		l.color = d2themes.ResolveThemeColor(theme, c.Color)
		c.Label, c.Language = "", ""
	}
}

func shapeLabelOrigin(s *d2target.Shape) vec {
	position := label.FromString(s.LabelPosition)
	if position == label.Unset {
		position = label.InsideMiddleCenter
	}
	outline := d2shape.NewShape(d2target.DSL_SHAPE_TO_SHAPE_TYPE[s.Type], geo.NewBox(geo.NewPoint(float64(s.Pos.X), float64(s.Pos.Y)), float64(s.Width), float64(s.Height)))
	if s.ContentAspectRatio != nil && s.Type == d2target.ShapeCloud {
		outline.SetInnerBoxAspectRatio(*s.ContentAspectRatio)
	}
	box := outline.GetInnerBox()
	if position.IsOutside() {
		box = outline.GetBox()
	}
	at := position.GetPointOnBox(box, label.PADDING, float64(s.LabelWidth), float64(s.LabelHeight))
	return vec{at.X, at.Y}
}

func (r *richLabels) overlays() map[string]overlay {
	out := make(map[string]overlay, len(r.byID))
	for id, l := range r.byID {
		var b strings.Builder
		if l.latex {
			l.writeFormula(&b)
		} else {
			l.markdown.write(&b, l.at, l.color)
		}
		out[base64.URLEncoding.EncodeToString([]byte(svg.EscapeText(id)))] = overlay{svg: []byte(b.String()), label: l.semanticLabel()}
	}
	return out
}

func (l *richLabel) semanticLabel() string {
	if l.latex {
		return strings.TrimSpace(l.source)
	}
	return ""
}

func (l *richLabel) writeFormula(b *strings.Builder) {
	_ = l.formula.Paths(func(path []figmath.Segment) error {
		b.WriteString(`<path d="`)
		writeSegments(b, path, l.at)
		fmt.Fprintf(b, `" fill="%s"/>`, svg.EscapeText(l.color))
		return nil
	})
}

func writeSegments(b *strings.Builder, path []figmath.Segment, at vec) {
	point := func(p figmath.Point) string {
		return fmt.Sprintf("%.3f %.3f", float64(p.X)+at.x, float64(p.Y)+at.y)
	}
	for i, s := range path {
		switch s.Op {
		case opentype.SegmentOpMoveTo:
			if i > 0 {
				b.WriteString("Z ")
			}
			b.WriteString("M" + point(s.Points[0]) + " ")
		case opentype.SegmentOpLineTo:
			b.WriteString("L" + point(s.Points[0]) + " ")
		case opentype.SegmentOpQuadTo:
			b.WriteString("Q" + point(s.Points[0]) + " " + point(s.Points[1]) + " ")
		case opentype.SegmentOpCubeTo:
			b.WriteString("C" + point(s.Points[0]) + " " + point(s.Points[1]) + " " + point(s.Points[2]) + " ")
		}
	}
	b.WriteString("Z")
}

func (d *decoder) overlay(n *svgNode, st renderState) error {
	if len(d.opts.overlays) == 0 {
		return nil
	}
	class, _ := n.attr("class")
	fields := strings.Fields(class)
	if len(fields) == 0 {
		return nil
	}
	o, ok := d.opts.overlays[fields[0]]
	if !ok {
		return nil
	}
	fragment := make([]byte, 0, len(o.svg)+64)
	fragment = append(fragment, `<svg xmlns="http://www.w3.org/2000/svg">`...)
	fragment = append(append(fragment, o.svg...), "</svg>"...)
	root, err := parseTree(fragment)
	if err != nil {
		return err
	}
	order := 0
	if err := d.visitTree(root, &order); err != nil {
		return err
	}
	if o.label != "" {
		d.list.Labels = append(d.list.Labels, o.label)
	}
	return d.children(root, st)
}
