package math

import (
	"image"
	stdmath "math"
	"slices"
	"testing"
)

type expectedNode struct {
	role, label, source string
}

func expressionOf(t *testing.T, source string) (*Layout, []ExpressionNode) {
	t.Helper()
	out := render(t, source, false)
	nodes := out.Expression()
	for i, node := range nodes {
		if node.Parent >= i || (i == 0) != (node.Parent == -1) {
			t.Fatalf("node %d has parent %d; parents must precede children and only the root has none", i, node.Parent)
		}
	}
	return out, nodes
}

func childrenOf(nodes []ExpressionNode, parent int) []int {
	var children []int
	for i, node := range nodes {
		if node.Parent == parent && i > 0 {
			children = append(children, i)
		}
	}
	return children
}

func expectChildren(t *testing.T, nodes []ExpressionNode, parent int, want []expectedNode) []int {
	t.Helper()
	children := childrenOf(nodes, parent)
	var got []expectedNode
	for _, i := range children {
		got = append(got, expectedNode{nodes[i].Role, nodes[i].Label, nodes[i].Source})
	}
	if !slices.Equal(got, want) {
		t.Fatalf("children of %d = %+v, want %+v", parent, got, want)
	}
	return children
}

const spokenSource = `\frac{a+1}{x^2}=\sqrt{y}\tag{3}`

func TestExpressionIsANavigableSpokenTree(t *testing.T) {
	_, nodes := expressionOf(t, spokenSource)
	root := nodes[0]
	if root.Role != "formula" || root.Source != spokenSource {
		t.Fatalf("formula root = %+v, want the formula role and the exact source", root)
	}
	if want := "fraction a + 1 over x superscript 2 end fraction = square root of y end root tag (3)"; root.Label != want {
		t.Fatalf("formula label %q, want %q", root.Label, want)
	}
	top := expectChildren(t, nodes, 0, []expectedNode{
		{"fraction", "fraction a + 1 over x superscript 2 end fraction", `\frac{a+1}{x^2}`},
		{"operator", "=", "="},
		{"square root", "square root of y end root", `\sqrt{y}`},
		{"tag", "tag (3)", `\tag{3}`},
	})
	parts := expectChildren(t, nodes, top[0], []expectedNode{{"numerator", "a + 1", "a+1"}, {"denominator", "x superscript 2", "x^2"}})
	expectChildren(t, nodes, parts[0], []expectedNode{{"identifier", "a", "a"}, {"operator", "+", "+"}, {"number", "1", "1"}})
	scripts := expectChildren(t, nodes, parts[1], []expectedNode{{"scripts", "x superscript 2", "x^2"}})
	expectChildren(t, nodes, scripts[0], []expectedNode{{"base", "x", "x"}, {"superscript", "2", "2"}})
}

func TestExpressionBoundsFollowTheLayout(t *testing.T) {
	out, nodes := expressionOf(t, spokenSource)
	fraction := childrenOf(nodes, 0)[0]
	parts := childrenOf(nodes, fraction)
	letter := nodes[childrenOf(nodes, parts[0])[0]]
	glyph := only(t, out, "a")
	want := pixelArea(glyph.X, out.Ascent+glyph.Y-450*script, glyph.X+500*script, out.Ascent+glyph.Y)
	if letter.Bounds != want {
		t.Errorf("numerator a bounds %v, want %v from its glyph box", letter.Bounds, want)
	}
	numerator, denominator := nodes[parts[0]].Bounds, nodes[parts[1]].Bounds
	if !numerator.Union(denominator).In(nodes[fraction].Bounds) || !nodes[fraction].Bounds.In(nodes[0].Bounds) {
		t.Errorf("numerator %v and denominator %v are not nested in the fraction %v inside %v", numerator, denominator, nodes[fraction].Bounds, nodes[0].Bounds)
	}
	if numerator.Empty() || numerator.Max.Y > denominator.Min.Y {
		t.Errorf("numerator %v is not stacked above denominator %v", numerator, denominator)
	}
}

func TestExpressionBoundsOfDelimitersAndEmptyCells(t *testing.T) {
	out, fenced := expressionOf(t, `\left( x \right)`)
	parts := childrenOf(fenced, childrenOf(fenced, 0)[0])
	opening := image.Rect(0, int(stdmath.Floor(float64(out.Ascent-750))), 400, int(stdmath.Ceil(float64(out.Ascent+250))))
	if len(parts) != 3 || fenced[parts[0]].Bounds != opening {
		t.Fatalf("fence parts %v, want the opening delimiter at %v", parts, opening)
	}
	_, table := expressionOf(t, `\begin{aligned}a&\\b&c\end{aligned}`)
	row := childrenOf(table, childrenOf(table, 0)[0])[0]
	cells := childrenOf(table, row)
	if len(cells) != 2 || table[cells[1]].Label != "" || table[cells[1]].Bounds != table[row].Bounds || table[row].Bounds.Empty() {
		t.Fatalf("empty cell %+v does not take its row bounds %v", table[cells[len(cells)-1]], table[row].Bounds)
	}
}

func TestExpressionNamesEveryConstruct(t *testing.T) {
	cases := []struct{ source, label string }{
		{`\sum_{i=1}^{n} x_i`, "∑ from i = 1 to n x subscript i"},
		{`f''`, "f double prime"},
		{`x_{ab}^{2}`, "x subscript a b end subscript superscript 2"},
		{`\sqrt[3]{x}`, "root with index 3 of x end root"},
		{`\binom{n}{k}`, "binomial n choose k end binomial"},
		{`\left( x \middle| y \right.`, "( x | y"},
		{`\hat{x}+\vec{v}`, "x hat + v vector"},
		{`\mathbb{R}\setminus\{0\}`, "double-struck R ∖ { 0 }"},
		{`\sin x`, "sin x"},
		{`\text{if } x`, "if  x"},
		{`\begin{pmatrix}a&b\\c&d\end{pmatrix}`, "begin pmatrix row 1 a b row 2 c d end pmatrix"},
	}
	for _, tc := range cases {
		if _, nodes := expressionOf(t, tc.source); nodes[0].Label != tc.label {
			t.Errorf("%q spoken as %q, want %q", tc.source, nodes[0].Label, tc.label)
		}
	}
	_, table := expressionOf(t, `\begin{align}a&=b\tag{2}\end{align}`)
	rows := childrenOf(table, childrenOf(table, 0)[0])
	if len(rows) != 1 || table[rows[0]].Role != "row" {
		t.Fatalf("aligned table rows %v", rows)
	}
	expectChildren(t, table, rows[0], []expectedNode{{"cell", "a", "a"}, {"cell", "= b", "=b"}, {"tag", "tag (2)", `\tag{2}`}})
}
