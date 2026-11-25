package math

const (
	strutHeightEm = 0.84
	strutDepthEm  = 0.36
)

type arrayFormat struct {
	level     uint8
	align     byte
	columnGap float32
	pairGap   float32
	pad       float32
	strut     float32
	rowGap    float32
	pairs     bool
}

var (
	matrixFormat = arrayFormat{level: levelText, align: 'c', columnGap: 1, strut: 1}
	alignFormat  = arrayFormat{level: levelDisplay, pairGap: 2, strut: 1, rowGap: 0.3, pairs: true}
	gatherFormat = arrayFormat{level: levelDisplay, align: 'c', strut: 1, rowGap: 0.3}
	arrayFormats = map[string]arrayFormat{
		"matrix": matrixFormat, "pmatrix": matrixFormat, "bmatrix": matrixFormat, "Bmatrix": matrixFormat,
		"vmatrix": matrixFormat, "Vmatrix": matrixFormat,
		"smallmatrix": {level: levelScript, align: 'c', columnGap: 0.5, strut: 1},
		"array":       {level: levelText, align: 'c', columnGap: 1, pad: 0.5, strut: 1},
		"cases":       {level: levelText, align: 'l', columnGap: 1, strut: 1.2},
		"aligned":     alignFormat, "align": alignFormat, "align*": alignFormat,
		"alignedat": {level: levelDisplay, strut: 1, rowGap: 0.3, pairs: true},
		"gathered":  gatherFormat, "gather": gatherFormat, "gather*": gatherFormat,
		"equation": gatherFormat, "equation*": gatherFormat,
	}
)

type arraySpec struct {
	aligns []byte
	rules  []int
}

func parseArraySpec(env *Environment, format arrayFormat, columns int) arraySpec {
	spec := arraySpec{aligns: make([]byte, columns), rules: make([]int, columns+1)}
	if env.Columns == "" {
		for i := range spec.aligns {
			spec.aligns[i] = format.columnAlign(i)
		}
		return spec
	}
	column := 0
	for i := range len(env.Columns) {
		if env.Columns[i] == '|' {
			spec.rules[column]++
			continue
		}
		if column < columns {
			spec.aligns[column] = env.Columns[i]
		}
		column++
	}
	return spec
}

func (f arrayFormat) columnAlign(column int) byte {
	switch {
	case !f.pairs:
		return f.align
	case column%2 == 0:
		return 'r'
	}
	return 'l'
}

func (f arrayFormat) gapBefore(column int) float32 {
	switch {
	case column == 0:
		return 0
	case !f.pairs:
		return f.columnGap
	case column%2 == 1:
		return 0
	}
	return f.pairGap
}

func environmentColumns(env *Environment) int {
	columns := 0
	if env.Columns != "" {
		for i := range len(env.Columns) {
			if env.Columns[i] != '|' {
				columns++
			}
		}
	}
	for _, row := range env.Rows {
		columns = max(columns, len(row.Cells))
	}
	return columns
}

type grid struct {
	cells      [][]*box
	widths     []float32
	heights    []float32
	depths     []float32
	baselines  []float32
	positions  []float32
	width      float32
	top, total float32
}

func (l *layouter) environment(n *Environment, st style) (*box, Class) {
	format := arrayFormats[n.Name]
	cellStyle := style{level: format.level, alphabet: st.alphabet}
	columns := environmentColumns(n)
	spec := parseArraySpec(n, format, columns)
	g := l.measureGrid(n, format, cellStyle, columns)
	l.placeRows(&g, format, cellStyle, st)
	l.placeColumns(&g, format, cellStyle)
	table := &box{width: g.width}
	for r, cells := range g.cells {
		for c, cell := range cells {
			table.add(cell, g.positions[c]+alignOffset(spec.aligns[c], g.widths[c], cell.width), g.baselines[r])
		}
	}
	table.height, table.depth = max(table.height, g.top), max(table.depth, g.total-g.top)
	l.drawColumnRules(table, &g, spec, st)
	l.placeTags(table, n, &g)
	kind := environments[n.Name]
	if kind.open == 0 && kind.close == 0 {
		return table, ClassOrd
	}
	return l.fenced(table, kind.open, kind.close, st), ClassInner
}

func alignOffset(align byte, column, cell float32) float32 {
	switch align {
	case 'r':
		return column - cell
	case 'c':
		return (column - cell) / 2
	}
	return 0
}

func (l *layouter) measureGrid(n *Environment, format arrayFormat, cellStyle style, columns int) grid {
	g := grid{cells: make([][]*box, len(n.Rows)), widths: make([]float32, columns)}
	for r, row := range n.Rows {
		g.cells[r] = make([]*box, len(row.Cells))
		for c, nodes := range row.Cells {
			if format.pairs && c%2 == 1 {
				nodes = append([]Node{&Group{}}, nodes...)
			}
			g.cells[r][c] = l.row(nodes, cellStyle)
			g.widths[c] = max(g.widths[c], g.cells[r][c].width)
		}
	}
	return g
}

func (l *layouter) placeRows(g *grid, format arrayFormat, cellStyle, outer style) {
	em := l.em(cellStyle)
	g.heights = make([]float32, len(g.cells))
	g.depths = make([]float32, len(g.cells))
	g.baselines = make([]float32, len(g.cells))
	var y float32
	for r, cells := range g.cells {
		g.heights[r], g.depths[r] = strutHeightEm*format.strut*em, strutDepthEm*format.strut*em
		for _, cell := range cells {
			g.heights[r] = max(g.heights[r], cell.height)
			g.depths[r] = max(g.depths[r], cell.depth)
		}
		if r > 0 {
			y -= format.rowGap * em
		}
		g.baselines[r] = y - g.heights[r]
		y = g.baselines[r] - g.depths[r]
	}
	g.total = -y
	g.top = g.total/2 + l.value(outer, axisHeight)
	for r := range g.baselines {
		g.baselines[r] += g.top
	}
}

func (l *layouter) placeColumns(g *grid, format arrayFormat, cellStyle style) {
	em := l.em(cellStyle)
	g.positions = make([]float32, len(g.widths))
	x := format.pad * em
	for c, width := range g.widths {
		x += format.gapBefore(c) * em
		g.positions[c] = x
		x += width
	}
	g.width = x + format.pad*em
}

func (g *grid) boundary(column int) float32 {
	switch {
	case column == 0:
		return 0
	case column >= len(g.widths):
		return g.width
	}
	return (g.positions[column-1] + g.widths[column-1] + g.positions[column]) / 2
}

func (l *layouter) drawColumnRules(table *box, g *grid, spec arraySpec, st style) {
	thickness := l.value(st, fractionRuleThickness)
	for column, count := range spec.rules {
		x := g.boundary(column) - float32(2*count-1)*thickness/2
		for i := range count {
			rule := l.ruleBox(thickness, g.total)
			table.add(rule, x+float32(2*i)*thickness, g.top-g.total)
		}
	}
}

func (l *layouter) placeTags(table *box, n *Environment, g *grid) {
	for r, row := range n.Rows {
		if row.Tag == nil {
			continue
		}
		text := l.textBox(tagLabel(row.Tag), style{level: levelText})
		text.span = row.Tag.Span
		x := g.width + tagGapEm*l.size
		table.add(text, x, g.baselines[r])
		table.width = max(table.width, x+text.width)
	}
}
