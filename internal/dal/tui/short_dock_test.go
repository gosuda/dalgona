package tui

import (
	"strings"
	"testing"
)

func shortDockRows(panel int, live, band bool) int {
	rows := 1 + panel
	if live {
		rows++
	}
	if band {
		rows++
	}
	return rows
}

func checkShortDock(t *testing.T, lines []string, title string, panel int, live, band bool) string {
	t.Helper()
	if want := shortDockRows(panel, live, band); len(lines) != want {
		t.Fatalf("dock has %d rows, want %d:\n%s", len(lines), want, strings.Join(lines, "\n"))
	}
	if !strings.HasPrefix(lines[0], "›") {
		t.Fatalf("row 0 is not the input line: %q", lines[0])
	}
	if !strings.HasPrefix(lines[1], title) {
		t.Fatalf("row 1 is not the title: %q", lines[1])
	}
	if live && !strings.Contains(lines[1+panel], "Waiting") {
		t.Fatalf("row %d is not the live row: %q", 1+panel, lines[1+panel])
	}
	if band && !strings.Contains(lines[len(lines)-1], "⊡") {
		t.Fatalf("last row is not the status band: %q", lines[len(lines)-1])
	}
	return strings.Join(lines[1:1+panel], "\n")
}
