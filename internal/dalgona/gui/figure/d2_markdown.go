package figure

import (
	"fmt"
	"math"
	"strconv"
	"strings"

	"oss.terrastruct.com/d2/d2renderers/d2fonts"
	"oss.terrastruct.com/d2/lib/svg"
	"oss.terrastruct.com/d2/lib/textmeasure"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
)

type markdownRun struct {
	text   string
	x, y   float64
	size   int
	weight int
	italic bool
	mono   bool
}

type markdownRect struct {
	x, y, width, height float64
	alpha               float64
	full                bool
}

type markdownLayout struct {
	runs   []markdownRun
	rects  []markdownRect
	width  float64
	height float64
}

type markdownPen struct {
	ruler  *textmeasure.Ruler
	base   int
	layout *markdownLayout
	y      float64
}

var headingScale = [...]float64{1: 2, 2: 1.5, 3: 1.25, 4: 1, 5: 0.875, 6: 0.85}

const (
	paragraphLeading = 1.5
	headingLeading   = 1.25
	codeLeading      = 1.45
	codeScale        = 0.85
	tableRuleAlpha   = 0.3
	shadeAlpha       = 0.1
)

func layoutMarkdown(source string, size int, ruler *textmeasure.Ruler) (*markdownLayout, error) {
	identity, err := document.LocalDraft("figure", "", "d2-label")
	if err != nil {
		return nil, err
	}
	doc, err := document.New(document.Header{Identity: identity, Format: document.FormatMarkdown}, source)
	if err != nil {
		return nil, err
	}
	p := &markdownPen{ruler: ruler, base: size, layout: &markdownLayout{}}
	for i := range doc.Len() {
		if i > 0 {
			p.y += p.em(1)
		}
		p.block(doc.Block(i))
	}
	p.layout.height = p.y
	for i := range p.layout.rects {
		if r := &p.layout.rects[i]; r.full {
			r.width = math.Max(p.layout.width-r.x, 0)
		}
	}
	return p.layout, nil
}

func (p *markdownPen) em(k float64) float64 { return k * float64(p.base) }

func (p *markdownPen) scaled(k float64) int { return max(int(math.Round(k*float64(p.base))), 1) }

func (p *markdownPen) block(b document.Block) {
	top := p.y
	indent, quotes := p.containers(b)
	switch b.Kind {
	case document.KindHeading:
		p.heading(b, indent)
	case document.KindCode, document.KindFigure, document.KindMath, document.KindDiff:
		p.code(b, indent)
	case document.KindRule:
		p.rule(indent)
	case document.KindTable:
		p.table(b, indent)
	case document.KindParagraph, document.KindHTML, document.KindNotice:
		p.paragraph(b.Projection, textStyle{indent: indent, size: p.base, weight: 400, leading: paragraphLeading})
	}
	for _, x := range quotes {
		p.layout.rects = append(p.layout.rects, markdownRect{x: x, y: top, width: p.em(0.25), height: p.y - top, alpha: tableRuleAlpha})
	}
}

func (p *markdownPen) containers(b document.Block) (float64, []float64) {
	indent := 0.0
	var quotes []float64
	for i, c := range b.Containers {
		if c.Kind == document.ContainerQuotation {
			quotes = append(quotes, indent)
			indent += p.em(1)
			continue
		}
		if c.First && i == len(b.Containers)-1 {
			p.marker(b, c, indent)
		}
		indent += p.em(2)
	}
	return indent, quotes
}

func (p *markdownPen) marker(b document.Block, c document.Container, indent float64) {
	text := "•"
	if c.Ordered {
		text = strconv.Itoa(c.Number) + string(rune(c.Marker))
	}
	switch b.Task {
	case document.TaskOpen:
		text = "[ ]"
	case document.TaskDone:
		text = "[x]"
	case document.TaskNone:
	}
	run := markdownRun{text: text, size: p.base, weight: 400}
	run.x = indent + p.em(1.6) - p.measure(run)
	run.y = p.baseline(p.base, paragraphLeading)
	p.place(run)
}

func (p *markdownPen) baseline(size int, leading float64) float64 {
	return p.y + float64(size)*leading/2 + 0.35*float64(size)
}

func (p *markdownPen) heading(b document.Block, indent float64) {
	level := min(max(b.Level, 1), len(headingScale)-1)
	p.paragraph(b.Projection, textStyle{indent: indent, size: p.scaled(headingScale[level]), weight: 700, leading: headingLeading})
	if level <= 2 {
		p.y += 0.3 * p.em(headingScale[level])
		p.layout.rects = append(p.layout.rects, markdownRect{x: indent, y: p.y, height: 1, alpha: tableRuleAlpha, full: true})
		p.y++
	}
}

type textStyle struct {
	indent  float64
	size    int
	weight  int
	leading float64
}

func (p *markdownPen) paragraph(pr document.Projection, style textStyle) {
	for _, line := range projectionLines(pr) {
		x := style.indent
		baseline := p.baseline(style.size, style.leading)
		for _, r := range line {
			run := markdownRun{text: pr.Text[r.Text.Start:r.Text.End], x: x, y: baseline, size: style.size, weight: style.weight}
			x += p.styled(run, r.Style)
		}
		p.layout.width = math.Max(p.layout.width, x)
		p.y += float64(style.size) * style.leading
	}
}

func projectionLines(pr document.Projection) [][]document.Run {
	lines := [][]document.Run{nil}
	for _, r := range pr.Runs {
		if r.Kind == document.Break && pr.Text[r.Text.Start:r.Text.End] == "\n" {
			lines = append(lines, nil)
			continue
		}
		lines[len(lines)-1] = append(lines[len(lines)-1], r)
	}
	return lines
}

func (p *markdownPen) styled(run markdownRun, style document.Style) float64 {
	if style&document.Strong != 0 {
		run.weight = 700
	}
	run.italic = style&document.Emphasis != 0
	if style&document.InlineCode != 0 {
		run.mono, run.size = true, max(int(math.Round(float64(run.size)*codeScale)), 1)
	}
	width := p.measure(run)
	size := float64(run.size)
	if run.mono {
		p.layout.rects = append(p.layout.rects, markdownRect{x: run.x, y: run.y - size, width: width, height: size * 1.3, alpha: shadeAlpha})
	}
	if style&document.Strikethrough != 0 {
		p.layout.rects = append(p.layout.rects, markdownRect{x: run.x, y: run.y - 0.3*size, width: width, height: math.Max(size/16, 1), alpha: 1})
	}
	if style&document.LinkText != 0 {
		p.layout.rects = append(p.layout.rects, markdownRect{x: run.x, y: run.y + 0.1*size, width: width, height: math.Max(size/16, 1), alpha: 1})
	}
	p.place(run)
	return width
}

func (p *markdownPen) place(run markdownRun) {
	if run.text == "" {
		return
	}
	p.layout.runs = append(p.layout.runs, run)
}

func (p *markdownPen) measure(run markdownRun) float64 {
	family := d2fonts.SourceSansPro
	if run.mono {
		family = d2fonts.SourceCodePro
	}
	style := d2fonts.FONT_STYLE_REGULAR
	switch {
	case run.weight >= 600:
		style = d2fonts.FONT_STYLE_BOLD
	case run.italic:
		style = d2fonts.FONT_STYLE_ITALIC
	}
	width, _ := p.ruler.MeasurePrecise(family.Font(run.size, style), run.text)
	return width
}

func (p *markdownPen) code(b document.Block, indent float64) {
	body := strings.TrimSuffix(b.Projection.Text[b.Body.Start:b.Body.End], "\n")
	size := p.scaled(codeScale)
	leading := float64(size) * codeLeading
	pad := p.em(1)
	top := p.y
	p.y += pad
	width := 0.0
	for _, line := range strings.Split(body, "\n") {
		run := markdownRun{text: line, x: indent + pad, y: p.baseline(size, codeLeading), size: size, weight: 400, mono: true}
		width = math.Max(width, p.measure(run))
		p.place(run)
		p.y += leading
	}
	p.y += pad
	p.layout.rects = append(p.layout.rects, markdownRect{x: indent, y: top, width: width + 2*pad, height: p.y - top, alpha: shadeAlpha})
	p.layout.width = math.Max(p.layout.width, indent+width+2*pad)
}

func (p *markdownPen) rule(indent float64) {
	p.layout.rects = append(p.layout.rects, markdownRect{x: indent, y: p.y, height: p.em(0.25), alpha: tableRuleAlpha, full: true})
	p.y += p.em(0.25)
}

func (p *markdownPen) table(b document.Block, indent float64) {
	rows := tableCells(b)
	widths := p.columnWidths(rows)
	padX, padY := p.em(13.0/16), p.em(6.0/16)
	rowHeight := float64(p.base)*paragraphLeading + 2*padY
	total := 0.0
	for _, w := range widths {
		total += w + 2*padX
	}
	for r, cells := range rows {
		x := indent
		for c, cell := range cells {
			run := markdownRun{text: cell, x: x + padX, y: p.y + padY + float64(p.base)*paragraphLeading/2 + 0.35*float64(p.base), size: p.base, weight: tableWeight(r)}
			p.place(run)
			x += widths[c] + 2*padX
		}
		p.layout.rects = append(p.layout.rects, markdownRect{x: indent, y: p.y, width: total, height: 1, alpha: tableRuleAlpha})
		p.y += rowHeight
	}
	p.layout.rects = append(p.layout.rects, markdownRect{x: indent, y: p.y, width: total, height: 1, alpha: tableRuleAlpha})
	p.layout.width = math.Max(p.layout.width, indent+total)
}

func tableWeight(row int) int {
	if row == 0 {
		return 700
	}
	return 400
}

func tableCells(b document.Block) [][]string {
	rows := make([][]string, 0, len(b.Rows))
	for _, span := range b.Rows {
		rows = append(rows, strings.Split(strings.TrimSuffix(b.Projection.Text[span.Start:span.End], "\n"), "\t"))
	}
	return rows
}

func (p *markdownPen) columnWidths(rows [][]string) []float64 {
	var widths []float64
	for r, cells := range rows {
		for c, cell := range cells {
			if c == len(widths) {
				widths = append(widths, 0)
			}
			widths[c] = math.Max(widths[c], p.measure(markdownRun{text: cell, size: p.base, weight: tableWeight(r)}))
		}
	}
	return widths
}

func (l *markdownLayout) write(b *strings.Builder, at vec, color string) {
	fill := svg.EscapeText(color)
	for _, r := range l.rects {
		fmt.Fprintf(b, `<rect x="%.3f" y="%.3f" width="%.3f" height="%.3f" fill="%s" fill-opacity="%.3f"/>`, at.x+r.x, at.y+r.y, r.width, r.height, fill, r.alpha)
	}
	for _, r := range l.runs {
		family, style := "sans-serif", "normal"
		if r.mono {
			family = "monospace"
		}
		if r.italic {
			style = "italic"
		}
		fmt.Fprintf(b, `<text x="%.3f" y="%.3f" fill="%s" style="font-size:%dpx;font-weight:%d;font-style:%s;font-family:%s;white-space:pre">%s</text>`,
			at.x+r.x, at.y+r.y, fill, r.size, r.weight, style, family, svg.EscapeText(r.text))
	}
}
