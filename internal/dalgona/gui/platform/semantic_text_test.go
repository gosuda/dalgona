package platform

import (
	"image"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

const geometryText = "Hi 😀 yo\nok"

func geometryNode() SemanticNode {
	return SemanticNode{ID: "reply", Role: RoleDocument, Name: "Reply", Value: geometryText, Bounds: image.Rect(0, 90, 200, 150), Lines: []TextLine{
		{Range: textinput.Range{Start: 0, End: 11}, Bounds: image.Rect(10, 100, 110, 120), Clusters: []ClusterSpan{{10, 20}, {20, 30}, {30, 40}, {40, 60}, {60, 70}, {70, 80}, {80, 90}, {90, 90}}},
		{Range: textinput.Range{Start: 11, End: 13}, Bounds: image.Rect(10, 120, 110, 140), Clusters: []ClusterSpan{{10, 20}, {20, 30}}},
	}}
}

func asciiLine(start, end, y int) TextLine {
	line := TextLine{Range: textinput.Range{Start: start, End: end}, Bounds: image.Rect(0, y, 10*(end-start), y+10)}
	for k := range end - start {
		line.Clusters = append(line.Clusters, ClusterSpan{Left: 10 * k, Right: 10*k + 10})
	}
	return line
}

func TestCheckSemanticsValidatesTextLines(t *testing.T) {
	if err := CheckSemantics([]SemanticNode{geometryNode()}); err != nil {
		t.Fatalf("a laid-out text was refused: %v", err)
	}
	cases := map[string]func(n *SemanticNode){
		"line splits an emoji":        func(n *SemanticNode) { n.Lines[0].Range.End = 5 },
		"line leaves the text":        func(n *SemanticNode) { n.Lines[1].Range.End = 14 },
		"line starts before the text": func(n *SemanticNode) { n.Lines[0].Range.Start = -1 },
		"inverted line box": func(n *SemanticNode) {
			n.Lines[1].Bounds = image.Rectangle{Min: image.Pt(110, 120), Max: image.Pt(10, 140)}
		},
		"missing cluster span":       func(n *SemanticNode) { n.Lines[1].Clusters = n.Lines[1].Clusters[:1] },
		"span outside the line box":  func(n *SemanticNode) { n.Lines[0].Clusters[0] = ClusterSpan{5, 20} },
		"span ends before it starts": func(n *SemanticNode) { n.Lines[0].Clusters[1] = ClusterSpan{30, 20} },
		"lines out of order":         func(n *SemanticNode) { n.Lines[0], n.Lines[1] = n.Lines[1], n.Lines[0] },
		"lines overlap": func(n *SemanticNode) {
			n.Lines[1] = TextLine{Range: textinput.Range{Start: 10, End: 13}, Bounds: n.Lines[1].Bounds, Clusters: []ClusterSpan{{10, 10}, {10, 20}, {20, 30}}}
		},
		"secure field line covers text": func(n *SemanticNode) {
			*n = SemanticNode{ID: "token", Role: RoleSecureField, Name: "Token", Action: "Edit", Lines: []TextLine{asciiLine(0, 1, 0)}}
		},
	}
	for name, breakNode := range cases {
		n := geometryNode()
		breakNode(&n)
		if err := CheckSemantics([]SemanticNode{n}); err == nil {
			t.Errorf("%s: the node was accepted", name)
		}
	}
}

func TestSemanticNodeMeasuresCharactersAndRanges(t *testing.T) {
	n := geometryNode()
	boxes := map[int]image.Rectangle{
		0:  image.Rect(10, 100, 20, 120),
		3:  image.Rect(40, 100, 60, 120),
		10: image.Rect(90, 100, 90, 120),
		11: image.Rect(10, 120, 20, 140),
		13: image.Rect(30, 120, 30, 140),
	}
	for offset, want := range boxes {
		if got, ok := n.characterBox(offset); !ok || got != want {
			t.Errorf("character box at %d = %v %t, want %v", offset, got, ok, want)
		}
	}
	if got := n.rangeBoxes(textinput.Range{Start: 1, End: 12}); !slices.Equal(got, []image.Rectangle{image.Rect(20, 100, 90, 120), image.Rect(10, 120, 20, 140)}) {
		t.Errorf("range boxes %v, want one box for each line part", got)
	}
	if got, ok := n.rangeBox(textinput.Range{Start: 1, End: 12}); !ok || got != image.Rect(10, 100, 90, 140) {
		t.Errorf("range box %v %t", got, ok)
	}
	if got := n.rangeBoxes(textinput.Range{Start: 3, End: 3}); len(got) != 0 {
		t.Errorf("an empty range has boxes %v", got)
	}
}

func TestTextRangeGeometryFallsBackToNodeBounds(t *testing.T) {
	n := geometryNode()
	n.Lines = nil
	if _, ok := n.characterBox(3); ok {
		t.Error("a node without line geometry reported a character box")
	}
	if got, ok := n.rangeBox(textinput.Range{Start: 0, End: 13}); !ok || got != n.Bounds {
		t.Errorf("a range without line geometry has box %v %t, want the node box", got, ok)
	}
	if got := n.rangeBoxes(textinput.Range{Start: 3, End: 3}); len(got) != 0 {
		t.Errorf("an empty range without line geometry has boxes %v", got)
	}
	n.Bounds = image.Rectangle{}
	if got := n.rangeBoxes(textinput.Range{Start: 0, End: 13}); len(got) != 0 {
		t.Errorf("an off-screen node without line geometry has boxes %v", got)
	}
}

func TestSemanticNodeFindsTheCharacterNearAPoint(t *testing.T) {
	n := geometryNode()
	points := map[image.Point]int{
		image.Pt(45, 110):  3,
		image.Pt(10, 100):  0,
		image.Pt(200, 110): 9,
		image.Pt(5, 130):   11,
		image.Pt(50, 300):  12,
		image.Pt(25, -40):  1,
	}
	for p, want := range points {
		line, ok := n.closestLine(p)
		if !ok {
			t.Fatalf("no line near %v", p)
		}
		if got := line.nearest(n.Value, p.X); got != want {
			t.Errorf("point %v maps to offset %d, want %d", p, got, want)
		}
	}
	n.Lines = nil
	if _, ok := n.closestLine(image.Pt(45, 110)); ok {
		t.Error("a node without line geometry found a line")
	}
}

func TestTextLinesPartitionTheTextAroundGeometry(t *testing.T) {
	type part struct {
		r       textinput.Range
		laidOut bool
	}
	cases := []struct {
		name  string
		value string
		lines []TextLine
		want  []part
	}{
		{"middle line laid out", "ab\ncd\nef", []TextLine{asciiLine(3, 6, 10)}, []part{{textinput.Range{Start: 0, End: 3}, false}, {textinput.Range{Start: 3, End: 6}, true}, {textinput.Range{Start: 6, End: 8}, false}}},
		{"geometry inside one hard line", "abcdefgh", []TextLine{asciiLine(2, 4, 0)}, []part{{textinput.Range{Start: 0, End: 2}, false}, {textinput.Range{Start: 2, End: 4}, true}, {textinput.Range{Start: 4, End: 8}, false}}},
		{"soft wrap", "abcdef", []TextLine{asciiLine(0, 3, 0), asciiLine(3, 6, 10)}, []part{{textinput.Range{Start: 0, End: 3}, true}, {textinput.Range{Start: 3, End: 6}, true}}},
		{"trailing break", "ab\n", nil, []part{{textinput.Range{Start: 0, End: 3}, false}, {textinput.Range{Start: 3, End: 3}, false}}},
		{"trailing break laid out", "ab\n", []TextLine{asciiLine(0, 3, 0)}, []part{{textinput.Range{Start: 0, End: 3}, true}, {textinput.Range{Start: 3, End: 3}, false}}},
		{"carriage return and line feed", "a\r\nb", nil, []part{{textinput.Range{Start: 0, End: 3}, false}, {textinput.Range{Start: 3, End: 4}, false}}},
		{"empty text", "", nil, []part{{textinput.Range{}, false}}},
	}
	for _, c := range cases {
		n := SemanticNode{ID: "t", Role: RoleDocument, Value: c.value, Lines: c.lines}
		if err := CheckSemantics([]SemanticNode{n}); err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		var got []part
		for line, laidOut := range n.textLines() {
			got = append(got, part{line.Range, laidOut})
		}
		if !slices.Equal(got, c.want) {
			t.Errorf("%s: lines %v, want %v", c.name, got, c.want)
		}
	}
}

func TestWordAndSentenceSegmentsFollowUnicodeBoundaries(t *testing.T) {
	const text = "Hello, world. 漢字テスト 한국어!"
	words := map[int]string{0: "Hello, ", 5: "Hello, ", 7: "world. ", 14: "漢", 17: "字", 23: "テスト ", 33: "한국어!", len(text): "한국어!"}
	for at, want := range words {
		r, err := wordSegment(text, at, letterWord)
		if err != nil || text[r.Start:r.End] != want {
			t.Errorf("word at %d = %q (%v), want %q", at, text[r.Start:r.End], err, want)
		}
	}
	if _, err := wordSegment(text, 15, letterWord); err == nil {
		t.Error("an offset inside a scalar was accepted")
	}
	if r, err := wordSegment("", 0, letterWord); err != nil || r != (textinput.Range{}) {
		t.Errorf("word in empty text = %v, %v", r, err)
	}
}

func TestSentenceSegmentsFollowUnicodeBoundaries(t *testing.T) {
	const text = "Hello, world. 漢字テスト 한국어!"
	sentences := map[int]string{5: "Hello, world. ", 20: "漢字テスト 한국어!", len(text): "漢字テスト 한국어!"}
	for at, want := range sentences {
		r, err := sentenceSegment(text, at)
		if err != nil || text[r.Start:r.End] != want {
			t.Errorf("sentence at %d = %q (%v), want %q", at, text[r.Start:r.End], err, want)
		}
	}
}
