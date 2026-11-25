package document

import "sort"

type RowRef struct {
	Entry  int
	Block  int
	Row    int
	Hidden int
}

type rowKey struct {
	block  Key
	row    int
	hidden int
}

type Estimate func(ref RowRef, scope *Scope) float32

type Viewport struct {
	rows      []RowRef
	keys      []rowKey
	heights   []float32
	measured  []bool
	tree      []float64
	firstRow  []int
	top       float64
	height    float64
	following bool
	unseen    int
	entries   int
}

func NewViewport() *Viewport {
	return &Viewport{following: true}
}

func (v *Viewport) Sync(scope *Scope, estimate Estimate) {
	carried := make(map[rowKey]float32, len(v.keys))
	for i, key := range v.keys {
		if v.measured[i] {
			carried[key] = v.heights[i]
		}
	}
	v.rows, v.keys, v.firstRow = v.rows[:0], v.keys[:0], v.firstRow[:0]
	for e := range scope.entries {
		v.firstRow = append(v.firstRow, len(v.rows))
		v.appendEntry(scope, e)
	}
	v.firstRow = append(v.firstRow, len(v.rows))
	v.heights = make([]float32, len(v.rows))
	v.measured = make([]bool, len(v.rows))
	for i, ref := range v.rows {
		height, ok := carried[v.keys[i]]
		if !ok {
			height = estimate(ref, scope)
		}
		v.heights[i], v.measured[i] = height, ok
	}
	if !v.following && len(scope.entries) > v.entries {
		v.unseen += len(scope.entries) - v.entries
	}
	v.entries = len(scope.entries)
	v.rebuild()
}

func (v *Viewport) appendEntry(scope *Scope, e int) {
	doc := scope.entries[e].doc
	if doc == nil {
		v.appendRow(RowRef{Entry: e, Block: -1}, rowKey{block: Key{Document: scope.entries[e].ref.Header.Identity}, row: -1})
		return
	}
	folds := scope.folded(doc)
	for b := 0; b < len(doc.blocks); {
		for len(folds) > 0 && folds[0].Blocks.Start < b {
			folds = folds[1:]
		}
		if len(folds) > 0 && folds[0].Blocks.Start == b {
			v.appendRow(RowRef{Entry: e, Block: b, Hidden: folds[0].Rows - 1}, rowKey{block: doc.blocks[b].Key, hidden: folds[0].Rows - 1})
			b = folds[0].Blocks.End
			continue
		}
		for r := range doc.blocks[b].Rows {
			v.appendRow(RowRef{Entry: e, Block: b, Row: r}, rowKey{block: doc.blocks[b].Key, row: r})
		}
		b++
	}
}

func (v *Viewport) appendRow(ref RowRef, key rowKey) {
	v.rows = append(v.rows, ref)
	v.keys = append(v.keys, key)
}

func (v *Viewport) rebuild() {
	v.tree = make([]float64, len(v.heights)+1)
	for i, h := range v.heights {
		v.tree[i+1] += float64(h)
		if parent := i + 1 + (i+1)&-(i+1); parent < len(v.tree) {
			v.tree[parent] += v.tree[i+1]
		}
	}
}

func (v *Viewport) Rows() int        { return len(v.rows) }
func (v *Viewport) Row(i int) RowRef { return v.rows[i] }
func (v *Viewport) Height(i int) float32 {
	return v.heights[i]
}
func (v *Viewport) Following() bool { return v.following }
func (v *Viewport) Unseen() int     { return v.unseen }
func (v *Viewport) Offset() float64 { return v.top }

func (v *Viewport) Total() float64 {
	return v.prefix(len(v.heights))
}

func (v *Viewport) prefix(n int) float64 {
	sum := 0.0
	for i := n; i > 0; i -= i & -i {
		sum += v.tree[i]
	}
	return sum
}

func (v *Viewport) RowTop(i int) float64 {
	return v.prefix(i)
}

func (v *Viewport) RowAt(y float64) int {
	if len(v.heights) == 0 {
		return 0
	}
	at, remaining := 0, y
	step := 1
	for step*2 <= len(v.heights) {
		step *= 2
	}
	for ; step > 0; step /= 2 {
		if next := at + step; next <= len(v.heights) && v.tree[next] <= remaining {
			at, remaining = next, remaining-v.tree[next]
		}
	}
	return min(at, len(v.heights)-1)
}

func (v *Viewport) Measure(i int, height float32) {
	delta := float64(height - v.heights[i])
	v.heights[i], v.measured[i] = height, true
	for j := i + 1; j < len(v.tree); j += j & -j {
		v.tree[j] += delta
	}
}

func (v *Viewport) Resize(height float64) {
	v.height = height
	v.clamp()
}

func (v *Viewport) clamp() {
	v.top = max(0, min(v.top, v.Total()-v.height))
}

func (v *Viewport) ScrollBy(dy float64) {
	v.top += dy
	v.clamp()
	v.following = v.Total()-(v.top+v.height) <= FollowDistance
	if v.following {
		v.unseen = 0
	}
}

func (v *Viewport) JumpToLatest() {
	v.following, v.unseen = true, 0
	v.top = max(0, v.Total()-v.height)
}

func (v *Viewport) Window() (int, int) {
	if len(v.rows) == 0 {
		return 0, 0
	}
	first := v.RowAt(max(0, v.top-v.height))
	bottom := v.top + 2*v.height
	last := v.RowAt(bottom)
	if v.RowTop(last) < bottom {
		last++
	}
	return first, min(max(last, first+1), len(v.rows))
}

func (v *Viewport) Locate(scope *Scope, p Position) (int, bool) {
	e, ok := scope.Index(p.Document)
	if !ok || e+1 >= len(v.firstRow) {
		return 0, false
	}
	doc := scope.entries[e].doc
	if doc == nil {
		return v.firstRow[e], true
	}
	b, t, err := doc.locate(p)
	if err != nil {
		return 0, false
	}
	rows := doc.blocks[b].Rows
	r := sort.Search(len(rows), func(r int) bool { return rows[r].End > t || r == len(rows)-1 })
	at := sort.Search(v.firstRow[e+1]-v.firstRow[e], func(i int) bool {
		ref := v.rows[v.firstRow[e]+i]
		return ref.Block > b || (ref.Block == b && ref.Row > r)
	})
	return v.firstRow[e] + max(at-1, 0), true
}

func (v *Viewport) Capture(row int, p Position, lineTop float32) Anchor {
	return Anchor{Position: p, Displacement: float32(v.RowTop(row) + float64(lineTop) - v.top)}
}

func (v *Viewport) Restore(scope *Scope, anchor Anchor, lineTop func(row int, p Position) float32) {
	if v.following {
		v.top = max(0, v.Total()-v.height)
		return
	}
	row, ok := v.Locate(scope, anchor.Position)
	if !ok {
		v.clamp()
		return
	}
	v.top = v.RowTop(row) + float64(lineTop(row, anchor.Position)) - float64(anchor.Displacement)
	v.clamp()
}

func (v *Viewport) DragScroll(pointer float64, seconds float64) {
	overshoot := 0.0
	switch {
	case pointer < 0:
		overshoot = pointer
	case pointer > v.height:
		overshoot = pointer - v.height
	}
	speed := max(-MaxDragScrollSpeed, min(MaxDragScrollSpeed, overshoot*10))
	if speed != 0 {
		v.ScrollBy(speed * seconds)
	}
}
