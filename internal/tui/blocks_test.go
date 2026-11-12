package tui

import (
	"fmt"
	"image/color"
	"reflect"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
)

func TestBlockSplitterParagraph(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "a\nb\n\nc")
	want := []block{{text: "a\nb"}}
	if !reflect.DeepEqual(final, want) || tail != "c" {
		t.Fatalf("paragraph: final %#v tail %q, want %#v tail %q", final, tail, want, "c")
	}
	if got := s.Close(); !reflect.DeepEqual(got, []block{{text: "c"}}) {
		t.Fatalf("paragraph close: %#v", got)
	}
}

func TestBlockSplitterFenceSpansDeltas(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "```go\nx := 1\n")
	want := []block{{text: "```go", code: true}, {text: "x := 1", code: true}}
	if !reflect.DeepEqual(final, want) || tail != "" {
		t.Fatalf("fence open: final %#v tail %q", final, tail)
	}
	final, tail = feedAll(&s, "``")
	if final != nil || tail != "``" {
		t.Fatalf("partial fence marker: final %#v tail %q", final, tail)
	}
	final, tail = feedAll(&s, "`\n")
	if !reflect.DeepEqual(final, []block{{text: "```", code: true}}) || tail != "" {
		t.Fatalf("fence close: final %#v tail %q", final, tail)
	}
}

func TestBlockSplitterList(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "- a\n- b\n\n")
	if final != nil || tail != "- b" {
		t.Fatalf("list before text: final %#v tail %q", final, tail)
	}
	final, tail = feedAll(&s, "text\n")
	if !reflect.DeepEqual(final, []block{{text: "- a\n- b"}}) || tail != "text" {
		t.Fatalf("list after text: final %#v tail %q", final, tail)
	}
}

func TestBlockSplitterTable(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "| a | b |\n|---|---|\n| 1 | 2 |\n\nnext\n")
	want := []block{{text: "| a | b |\n|---|---|\n| 1 | 2 |"}}
	if !reflect.DeepEqual(final, want) || tail != "next" {
		t.Fatalf("table: final %#v tail %q", final, tail)
	}
}

func TestBlockSplitterDeltaEndsInsideWord(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "hel")
	if final != nil || tail != "hel" {
		t.Fatalf("partial word: final %#v tail %q", final, tail)
	}
	final, tail = feedAll(&s, "lo\n\n")
	if !reflect.DeepEqual(final, []block{{text: "hello"}}) || tail != "" {
		t.Fatalf("finished word: final %#v tail %q", final, tail)
	}
}

func TestBlockSplitterHeadingAndRule(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "# T\ntext\n---\n")
	want := []block{{text: "# T"}, {text: "text"}, {text: "---"}}
	if !reflect.DeepEqual(final, want) || tail != "" {
		t.Fatalf("heading and rule: final %#v tail %q", final, tail)
	}
}

func TestBlockSplitterCloseFlushesPartialLine(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	feedAll(&s, "x")
	if got := s.Close(); !reflect.DeepEqual(got, []block{{text: "x"}}) {
		t.Fatalf("close of partial line: %#v", got)
	}
}

func TestStreamingMarkdownRendersClosedBlocks(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{}, nil)
	m.screen.width = 60
	m, _ = drive(t, m, tea.ColorProfileMsg{Profile: colorprofile.TrueColor})
	m, _ = drive(t, m, tea.BackgroundColorMsg{Color: color.Black})
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"# Title\n\nsome text\n\npartial **bo"}`)})
	var rows []string
	for _, entry := range m.out.entries {
		rows = append(rows, entry.text)
	}
	printed := ansi.Strip(strings.Join(rows, "\n"))
	if !strings.Contains(printed, "Title") || strings.Contains(printed, "# Title") {
		t.Fatalf("closed heading not printed as rendered text: %q", printed)
	}
	if !strings.Contains(printed, "some text") {
		t.Fatalf("closed paragraph not printed as text: %q", printed)
	}
	keys := entryKeys(m)
	first, second := slices.Index(keys, "block:t1:a1:0"), slices.Index(keys, "block:t1:a1:1")
	if first < 0 || second <= first {
		t.Fatalf("answer blocks keyed %q, want block:t1:a1:0 before block:t1:a1:1", keys)
	}
	if !strings.Contains(ansi.Strip(m.View().Content), "partial **bo") {
		t.Fatalf("live row lacks the unfinished tail:\n%s", m.View().Content)
	}
}

func answerThread(t *testing.T) model {
	t.Helper()
	m := newTestModel(nil, Options{}, nil)
	m.screen.width = 60
	m, _ = drive(t, m, tea.ColorProfileMsg{Profile: colorprofile.TrueColor})
	m, _ = drive(t, m, tea.BackgroundColorMsg{Color: color.Black})
	m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
	m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
	return m
}

func TestAnswerTextEqualsSanitizedFinal(t *testing.T) {
	t.Parallel()
	splits := [][]string{
		{`a\u001b`, `[31mred\n\nafter`},
		{`a\u001b[3`, `1mred\n\nafter`},
	}
	for _, deltas := range splits {
		m := answerThread(t)
		for _, delta := range deltas {
			m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"` + delta + `"}`)})
		}
		m, _ = drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":"a\u001b[31mred\n\nafter"}}`)})
		var rows []string
		for _, entry := range m.out.entries {
			rows = append(rows, entry.text)
		}
		printed := ansi.Strip(strings.Join(rows, "\n"))
		if !strings.Contains(printed, "ared") || !strings.Contains(printed, "after") || strings.Contains(printed, "[31m") {
			t.Fatalf("deltas %q printed %q, want the sanitized answer", deltas, printed)
		}
	}
}

func feedAll(s *blockSplitter, deltas ...string) ([]block, string) {
	var final []block
	var tail string
	for _, delta := range deltas {
		var step []block
		step, tail = s.Feed(delta)
		final = append(final, step...)
	}
	return final, tail
}

func TestDockHeightStableWhileAnswerStreams(t *testing.T) {
	t.Parallel()
	for _, size := range []tea.WindowSizeMsg{{Width: 40, Height: 8}, {Width: 100, Height: 24}} {
		m := newTestModel(nil, Options{}, nil)
		m, _ = drive(t, m, size)
		m, _ = drive(t, m, notificationMsg{method: "thread/started", params: []byte(`{"thread":{"id":"t1","name":"demo","preview":"","status":{"type":"active","activeFlags":[]},"turns":[]}}`)})
		m, _ = drive(t, m, notificationMsg{method: "item/started", params: []byte(`{"threadId":"t1","turnId":"turn-1","startedAtMs":1,"item":{"type":"agentMessage","id":"a1","text":""}}`)})
		rows := func() int { return strings.Count(strings.Join(m.dockLines(), "\n"), "\n") + 1 }
		height := rows()
		for i := range 200 {
			delta := fmt.Sprintf("word %d and more text here\n", i)
			if i%7 == 0 {
				delta += "\n```go\n"
			}
			m, _ = drive(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":%q}`, delta))})
			if got := rows(); got != height {
				t.Fatalf("width %d: dock height after delta %d = %d, want %d", size.Width, i, got, height)
			}
		}
	}
}
