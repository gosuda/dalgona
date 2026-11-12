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
	rows := approvalChoices(&screen, 0)
	if len(rows) != 2 {
		t.Fatalf("approval actions gave %d rows, want two", len(rows))
	}
	if got := screen.cells(rows[1]); got != ansi.WcWidth.StringWidth(rows[1]) {
		t.Fatalf("second action row measures %d, want the renderer width", got)
	}
}
