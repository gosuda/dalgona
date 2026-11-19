package tui

import (
	"fmt"
	"testing"
)

func TestPanelRowsCapAtWindowHeightMinusFour(t *testing.T) {
	t.Parallel()
	for kind := panelPopup; kind <= panelHelp; kind++ {
		if kind == panelApproval {
			continue
		}
		for height := 0; height <= 24; height++ {
			want := max(0, min(panelHeights[kind], height-4))
			if got := panelRows(kind, height); got != want {
				t.Fatalf("panelRows(%d, %d) = %d, want %d", kind, height, got, want)
			}
		}
	}
}

func TestApprovalPanelRowsKeepAFloor(t *testing.T) {
	t.Parallel()
	want := map[int]int{0: 0, 1: 0, 2: 0, 3: 0, 4: 2, 5: 3, 6: 3, 7: 3, 8: 4, 9: 5, 12: 8, 13: 9, 14: 9, 24: 9}
	for height, rows := range want {
		t.Run(fmt.Sprintf("height %d", height), func(t *testing.T) {
			t.Parallel()
			if got := panelRows(panelApproval, height); got != rows {
				t.Fatalf("panelRows(panelApproval, %d) = %d, want %d", height, got, rows)
			}
		})
	}
}
