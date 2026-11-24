//go:build gui

package document

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
)

func taskBlock(task Task) Block {
	b := styledParagraph("do it")
	b.Containers = []Container{{Kind: ContainerListItem, First: true}}
	b.Task = task
	return b
}

func TestShapeMarkerSkipsReadOnlyTaskItems(t *testing.T) {
	shaper := testShaper()
	typo := testTypography()
	for _, task := range []Task{TaskOpen, TaskDone} {
		rl := ShapeRow(testContext(400, 0), shaper, taskBlock(task), 0, typo)
		if len(rl.marker) != 0 {
			t.Fatalf("task state %v shaped %d bullet glyphs, want a checkbox instead", task, len(rl.marker))
		}
	}
	plain := styledParagraph("do it")
	plain.Containers = []Container{{Kind: ContainerListItem, First: true}}
	if n := len(ShapeRow(testContext(400, 0), shaper, plain, 0, typo).marker); n == 0 {
		t.Fatal("ordinary list item lost its bullet marker")
	}
}

func TestTokenKindResolvesHighlightSpans(t *testing.T) {
	rl := &RowLayout{tokens: []code.Span{{Start: 0, End: 3, Kind: code.Keyword}}}
	rl.SetTokens([]code.Span{{Start: 0, End: 3, Kind: code.Keyword}, {Start: 3, End: 8, Kind: code.String}})
	if got := rl.tokenKind(1); got != code.Keyword {
		t.Fatalf("tokenKind(1) = %v, want keyword", got)
	}
	if got := rl.tokenKind(4); got != code.String {
		t.Fatalf("tokenKind(4) = %v, want string", got)
	}
	if got := rl.tokenKind(8); got != code.PlainText {
		t.Fatalf("tokenKind(8) = %v, want plain text past the spans", got)
	}
	if got := (&RowLayout{}).tokenKind(0); got != code.PlainText {
		t.Fatalf("tokenKind without spans = %v, want plain text", got)
	}
}
