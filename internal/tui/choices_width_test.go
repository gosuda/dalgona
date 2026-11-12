package tui

import (
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestApprovalChoicesMeasureWithRendererMethod(t *testing.T) {
	t.Parallel()
	screen := screenState{method: ansi.WcWidth}
	family := "\U0001F468\u200d\U0001F469\u200d\U0001F467"
	if got, want := screen.cells(family), ansi.WcWidth.StringWidth(family); got != want {
		t.Fatalf("cells(family) = %d, want the renderer method width %d", got, want)
	}
	one := approvalChoices(&screen, 1000)
	if len(one) != 1 {
		t.Fatalf("wide window gave %d rows, want one", len(one))
	}
	width := screen.cells(one[0])
	if got := approvalChoices(&screen, width); len(got) != 1 {
		t.Fatalf("exact width gave %d rows, want one", len(got))
	}
	if got := approvalChoices(&screen, width-1); len(got) != 4 {
		t.Fatalf("one cell short gave %d rows, want four", len(got))
	}
}
