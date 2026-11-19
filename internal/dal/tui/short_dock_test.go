package tui

import (
	"strings"
	"testing"
)

type shortDock struct {
	panel int
	live  bool
	band  bool
}

func (d shortDock) rows() int {
	rows := 1 + d.panel
	if d.live {
		rows++
	}
	if d.band {
		rows++
	}
	return rows
}

func (d shortDock) check(t *testing.T, lines []string, title string) {
	t.Helper()
	if len(lines) != d.rows() {
		t.Fatalf("dock has %d rows, want %d:\n%s", len(lines), d.rows(), strings.Join(lines, "\n"))
	}
	if !strings.HasPrefix(lines[0], "›") {
		t.Fatalf("row 0 is not the input line: %q", lines[0])
	}
	if !strings.HasPrefix(lines[1], title) {
		t.Fatalf("row 1 is not the title: %q", lines[1])
	}
	if d.live && !strings.Contains(lines[1+d.panel], "Waiting") {
		t.Fatalf("row %d is not the live row: %q", 1+d.panel, lines[1+d.panel])
	}
	if d.band && !strings.Contains(lines[len(lines)-1], "⊡") {
		t.Fatalf("last row is not the status band: %q", lines[len(lines)-1])
	}
}
