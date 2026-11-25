package document

import (
	"sort"
	"strings"
)

type Style uint16

const (
	Strong Style = 1 << iota
	Emphasis
	Strikethrough
	InlineCode
	LinkText
	InlineMath
	RawMarkup
	ImageText
)

type RunKind uint8

const (
	Verbatim RunKind = iota + 1
	Decoded
	Break
	Generated
	Formula
)

type Run struct {
	Text   Span
	Source Span
	Kind   RunKind
	Style  Style
	Link   int
}

type Projection struct {
	Text string
	Runs []Run
}

type projector struct {
	source string
	text   strings.Builder
	runs   []Run
}

func (p *projector) add(source Span, text string, kind RunKind, style Style, link int) {
	if text == "" {
		return
	}
	start := p.text.Len()
	p.text.WriteString(text)
	run := Run{Text: Span{Start: start, End: p.text.Len()}, Source: source, Kind: kind, Style: style, Link: link}
	if n := len(p.runs); n > 0 && mergeable(p.runs[n-1], run) {
		p.runs[n-1].Text.End = run.Text.End
		p.runs[n-1].Source.End = run.Source.End
		return
	}
	p.runs = append(p.runs, run)
}

func mergeable(previous, next Run) bool {
	return previous.Kind == Verbatim && next.Kind == Verbatim &&
		previous.Style == next.Style && previous.Link == next.Link &&
		previous.Source.End == next.Source.Start
}

func (p *projector) verbatim(source Span, style Style, link int) {
	p.add(source, p.source[source.Start:source.End], Verbatim, style, link)
}

func (p *projector) projection() Projection {
	return Projection{Text: p.text.String(), Runs: p.runs}
}

func (p Projection) runAt(offset int) int {
	return sort.Search(len(p.Runs), func(i int) bool { return p.Runs[i].Text.End > offset })
}

func (p Projection) sourceOffset(offset int, affinity Affinity) int {
	i := p.runAt(offset)
	if affinity == Before && (i == len(p.Runs) || offset == p.Runs[i].Text.Start) {
		return p.sourceBefore(i)
	}
	if i == len(p.Runs) {
		return p.Runs[len(p.Runs)-1].Source.End
	}
	run := p.Runs[i]
	switch {
	case run.Kind == Verbatim:
		return run.Source.Start + offset - run.Text.Start
	case offset == run.Text.Start:
		return run.Source.Start
	default:
		return run.Source.End
	}
}

func (p Projection) sourceBefore(i int) int {
	for k := i - 1; k >= 0; k-- {
		if p.Runs[k].Source.Len() > 0 {
			return p.Runs[k].Source.End
		}
	}
	return p.Runs[0].Source.Start
}

func (p Projection) textOffset(source int, affinity Affinity) int {
	if affinity == Before {
		return p.textBefore(source)
	}
	return p.textAfter(source)
}

func (p Projection) textBefore(source int) int {
	i := sort.Search(len(p.Runs), func(i int) bool { return p.Runs[i].Source.Start >= source }) - 1
	if i < 0 {
		return 0
	}
	run := p.Runs[i]
	if source < run.Source.End && run.Kind == Verbatim {
		return run.Text.Start + source - run.Source.Start
	}
	return run.Text.End
}

func (p Projection) textAfter(source int) int {
	j := sort.Search(len(p.Runs), func(j int) bool { return p.Runs[j].Source.End > source })
	if j == len(p.Runs) {
		return len(p.Text)
	}
	run := p.Runs[j]
	if source > run.Source.Start && run.Kind == Verbatim {
		return run.Text.Start + source - run.Source.Start
	}
	return run.Text.Start
}

func (p Projection) insideAtomic(offset int) (Run, bool) {
	i := p.runAt(offset)
	if i == len(p.Runs) {
		return Run{}, false
	}
	run := p.Runs[i]
	return run, run.Kind != Verbatim && offset > run.Text.Start
}
