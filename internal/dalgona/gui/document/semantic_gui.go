//go:build gui

package document

import (
	"image"
	"math"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func (f *Flow) SemanticLines(span Span) []platform.TextLine {
	var lines []platform.TextLine
	for _, line := range f.Lines {
		start, end := max(line.Text.Start, span.Start), min(line.Text.End, span.End)
		if start >= end {
			continue
		}
		space := textinput.Range{Start: start - span.Start, End: end - span.Start}
		if textinput.CheckRange(f.Text[span.Start:span.End], space) != nil {
			continue
		}
		clusters := f.semanticClusters(line, Span{Start: start, End: end})
		if clusters == nil {
			continue
		}
		bounds := image.Rect(int(math.Floor(float64(f.Inset))), int(math.Floor(float64(line.Top))), int(math.Ceil(float64(f.Inset+line.Width))), int(math.Ceil(float64(line.Bottom))))
		lines = append(lines, platform.TextLine{Range: space, Bounds: bounds, Clusters: clusters})
	}
	return lines
}

func (f *Flow) semanticClusters(line Line, span Span) []platform.ClusterSpan {
	boundaries := textinput.Boundaries(f.Text[span.Start:span.End])
	out := make([]platform.ClusterSpan, 0, len(boundaries)-1)
	at := line.Clusters.Start
	for i, edge := range boundaries[:len(boundaries)-1] {
		lo, hi := span.Start+edge, span.Start+boundaries[i+1]
		for at < line.Clusters.End && f.Clusters[at].Text.End <= lo {
			at++
		}
		if at >= line.Clusters.End || lo < f.Clusters[at].Text.Start || hi > f.Clusters[at].Text.End {
			return nil
		}
		out = append(out, f.semanticCluster(at, lo, hi))
	}
	return out
}

func (f *Flow) semanticCluster(i, lo, hi int) platform.ClusterSpan {
	x := f.Inset + f.X[i]
	a, b := x+f.within(i, lo), x+f.within(i, hi)
	return platform.ClusterSpan{Left: int(math.Floor(float64(min(a, b)))), Right: int(math.Ceil(float64(max(a, b))))}
}
