package document

import (
	"maps"
	"sort"
)

type FoldKind uint8

const (
	FoldCode FoldKind = iota + 1
	FoldDiff
	FoldQuote
)

const LongCodeRows = 10

type Fold struct {
	Kind   FoldKind
	Key    Key
	Blocks Span
	Rows   int
	Start  Position
	Source Span
}

func (d *Document) Folds() []Fold {
	var folds []Fold
	for i, b := range d.blocks {
		if end, ok := d.quoteEnd(i); ok {
			if f := d.fold(FoldQuote, i, end); f.Rows >= 2 {
				folds = append(folds, f)
			}
		}
		if kind, ok := blockFold(b); ok {
			folds = append(folds, d.fold(kind, i, i+1))
		}
	}
	return folds
}

func blockFold(b Block) (FoldKind, bool) {
	switch {
	case b.Kind == KindCode && len(b.Rows) >= LongCodeRows:
		return FoldCode, true
	case b.Kind == KindDiff && len(b.Rows) >= 2:
		return FoldDiff, true
	}
	return 0, false
}

func (d *Document) quoteEnd(i int) (int, bool) {
	r := sort.Search(len(d.regions), func(r int) bool { return d.regions[r].first >= i })
	if r == len(d.regions) || d.regions[r].first != i {
		return 0, false
	}
	containers := d.blocks[i].Containers
	if len(containers) == 0 || containers[0].Kind != ContainerQuotation {
		return 0, false
	}
	for r+1 < len(d.regions) && d.regions[r+1].first == i {
		r++
	}
	if r+1 < len(d.regions) {
		return d.regions[r+1].first, true
	}
	return len(d.blocks), true
}

func (d *Document) fold(kind FoldKind, first, end int) Fold {
	rows := 0
	for _, b := range d.blocks[first:end] {
		rows += len(b.Rows)
	}
	return Fold{
		Kind:   kind,
		Key:    d.blocks[first].Key,
		Blocks: Span{Start: first, End: end},
		Rows:   rows,
		Start:  d.at(first, 0, After),
		Source: Span{Start: d.blocks[first].Span.Start, End: d.blocks[end-1].Span.End},
	}
}

func (s *Scope) Folded(key Key) bool {
	return s.folds[key]
}

func (s *Scope) toggleFold(key Key) {
	if s.folds[key] {
		delete(s.folds, key)
		return
	}
	if s.folds == nil {
		s.folds = map[Key]bool{}
	}
	s.folds[key] = true
}

func (s *Scope) folded(d *Document) []Fold {
	if len(s.folds) == 0 {
		return nil
	}
	var folded []Fold
	for _, f := range d.Folds() {
		if s.folds[f.Key] {
			folded = append(folded, f)
		}
	}
	return folded
}

func (st *State) ToggleFold(v *Viewport, fold Fold) {
	anchor := st.Anchor.Position
	if st.Anchored && anchor.Document == fold.Start.Document && fold.Source.contains(anchor.Offset) {
		if row, ok := v.Locate(st.Scope, fold.Start); ok {
			st.Anchor = v.Capture(row, fold.Start, 0)
			st.Anchor.Displacement = max(st.Anchor.Displacement, 0)
		}
	}
	st.Scope.toggleFold(fold.Key)
}

func remapKeys[V any](m map[Key]V, changes []KeyChange) map[Key]V {
	out := maps.Clone(m)
	for _, change := range changes {
		if value, ok := m[change.From]; ok {
			delete(out, change.From)
			out[change.To] = value
		}
	}
	return out
}
