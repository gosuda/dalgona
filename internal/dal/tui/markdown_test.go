package tui

import (
	"fmt"
	"image/color"
	"strconv"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
)

func TestAnswerBlockRowsStartAtTheGutter(t *testing.T) {
	t.Parallel()
	answer := "# Title\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nthe quick brown fox jumps over the lazy dog again and again until the very end of the line\n\n---\n"
	words := []string{"Title", "one", "a", "quick", "-"}
	for _, colour := range []bool{false, true} {
		for _, width := range []int{40, 100} {
			blocks := answerBlocks(t, answer, colour, width)
			if len(blocks) != len(words) {
				t.Fatalf("colour %v width %d: %d answer blocks, want %d: %q", colour, width, len(blocks), len(words), blocks)
			}
			for i, rows := range blocks {
				assertGutterBlock(t, fmt.Sprintf("colour %v width %d block %d", colour, width, i), rows, words[i], width)
			}
		}
	}
}

func answerBlocks(t *testing.T, answer string, colour bool, width int) [][]string {
	t.Helper()
	m := newTestModel(nil, Options{})
	m.screen.width = width
	m.input.composer.SetWidth(width)
	if colour {
		m, _ = drive(t, m, tea.ColorProfileMsg{Profile: colorprofile.TrueColor})
		m, _ = drive(t, m, tea.BackgroundColorMsg{Color: color.Black})
	}
	text := strconv.Quote(answer)
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":` + text + `}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":` + text + `}}`)})
	var blocks [][]string
	for _, entry := range m.out.entries {
		if strings.HasPrefix(entry.key, "block:t1:a1:") {
			blocks = append(blocks, strings.Split(entry.text, "\n"))
		}
	}
	return blocks
}

func assertGutterBlock(t *testing.T, label string, rows []string, word string, width int) {
	t.Helper()
	assertFrame(t, rows, width, len(rows)+1)
	if got := ansi.Strip(rows[0]); !strings.HasPrefix(got, "  ") || !strings.Contains(got[2:], word) {
		t.Fatalf("%s: first row %q, want %q after a 2-cell gutter", label, got, word)
	}
	if len(rows) < 2 || rows[len(rows)-1] != "" || strings.TrimSpace(ansi.Strip(rows[len(rows)-2])) == "" {
		t.Fatalf("%s: rows %q, want text then one blank spacer", label, rows)
	}
	for _, row := range rows {
		plain := ansi.Strip(row)
		if strings.TrimSpace(plain) != "" && !strings.HasPrefix(plain, "  ") {
			t.Fatalf("%s: row %q leaves the gutter", label, plain)
		}
	}
}
