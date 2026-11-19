package tui

import (
	"fmt"
	"testing"
)

func TestPanelRowsCapAtWindowHeightMinusFour(t *testing.T) {
	t.Parallel()
	for kind := panelPopup; kind <= panelHelp; kind++ {
		if kind == panelApproval || kind == panelAsk {
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

func TestDecisionPanelRowsKeepAFloor(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		kind panelKind
		want map[int]int
	}{
		{"approval", panelApproval, map[int]int{0: 0, 1: 0, 2: 0, 3: 0, 4: 2, 5: 3, 6: 3, 7: 3, 8: 4, 9: 5, 12: 8, 13: 9, 14: 9, 24: 9}},
		{"question", panelAsk, map[int]int{0: 0, 1: 0, 2: 0, 3: 0, 4: 2, 5: 3, 6: 3, 7: 3, 8: 4, 9: 5, 12: 8, 16: 12, 17: 13, 18: 13, 24: 13}},
	} {
		for height, rows := range tc.want {
			t.Run(fmt.Sprintf("%s height %d", tc.name, height), func(t *testing.T) {
				t.Parallel()
				if got := panelRows(tc.kind, height); got != rows {
					t.Fatalf("panelRows(%s, %d) = %d, want %d", tc.name, height, got, rows)
				}
			})
		}
	}
}
