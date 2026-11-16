package tui

import (
	"fmt"
	"image/color"
	"reflect"
	"slices"
	"strings"
	"testing"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"
)

const pendingLimit = 4 << 10

const tailLimit = 64 << 10

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
	m := newTestModel(nil, Options{})
	m.screen.width = 100
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
	if strings.Contains(printed, "partial **bo") {
		t.Fatalf("unfinished tail printed before its block closed: %q", printed)
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
	m := newTestModel(nil, Options{})
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
		m := newTestModel(nil, Options{})
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

func TestAnswerTailKeepsNewestCharsOfOversizedUTF8Line(t *testing.T) {
	t.Parallel()
	m := answerThread(t)
	line := "HEADMARKER-" + strings.Repeat("é", 40000) + "TAILMARKER-☃"
	m = rawUpdate(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":%q}`, line))})
	tail := m.liveItem("a1").tail()
	if !strings.HasSuffix(tail, "TAILMARKER-☃") {
		t.Fatalf("oversized line tail is %d bytes, want the newest chars ending in the tail marker", len(tail))
	}
	if strings.Contains(tail, "HEADMARKER") {
		t.Fatalf("oversized line tail kept the head: %d bytes", len(tail))
	}
	if !utf8.ValidString(tail) {
		t.Fatalf("oversized line tail split a rune: %d bytes", len(tail))
	}
	if len(tail) > tailLimit {
		t.Fatalf("oversized line tail is %d bytes, want at most %d", len(tail), tailLimit)
	}
}

func TestAnswerDropsUnterminatedControlWithBoundedState(t *testing.T) {
	t.Parallel()
	m := answerThread(t)
	send := func(delta string) {
		m = rawUpdate(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"` + delta + `"}`)})
	}
	unterminated := func(payload string) {
		send(`\u001b]0;`)
		send(payload)
	}
	assertPending := func(label string) {
		t.Helper()
		if got := len(m.liveItem("a1").pending); got > pendingLimit {
			t.Fatalf("pending %s is %d bytes, want at most %d", label, got, pendingLimit)
		}
	}
	assertVisibleSuffix := func(want string, absent string) {
		t.Helper()
		got := string(m.liveItem("a1").text)
		if !strings.Contains(got, want) || strings.Contains(got, absent) {
			t.Fatalf("text after terminator = %q, want the suffix without the payload", got)
		}
		if tail := m.liveItem("a1").tail(); !strings.Contains(tail, want) {
			t.Fatalf("live tail after terminator = %q, want the suffix", tail)
		}
	}
	unterminated(strings.Repeat("Q", 2000))
	unterminated(strings.Repeat("Q", 2000))
	assertPending("below the limit")
	send(strings.Repeat("Q", 100) + `\u001b`)
	assertPending("after unterminated control")
	if strings.Contains(string(m.liveItem("a1").text), "Q") {
		t.Fatalf("unterminated control payload reached visible text")
	}
	send(`\\VISIBLE-SUFFIX-7K2`)
	assertVisibleSuffix("VISIBLE-SUFFIX-7K2", "Q")
	unterminated(strings.Repeat("W", 5000))
	assertPending("after second unterminated control")
	send(`\u0007BEL-SUFFIX-3Q9`)
	assertVisibleSuffix("BEL-SUFFIX-3Q9", "W")
	send(`BEGIN-\u001b[31`)
	send(`mRED-TEXT-9D4`)
	if got := string(m.liveItem("a1").text); !strings.Contains(got, "RED-TEXT-9D4") || strings.Contains(got, "[31m") {
		t.Fatalf("fragmented CSI text = %q, want the visible text without controls", got)
	}
}

func TestAnswerDropsTerminatorAtFragmentEnd(t *testing.T) {
	t.Parallel()
	oversized := strings.Repeat("Z", 5000)
	cases := []struct {
		name      string
		fragments []string
	}{
		{"BEL", []string{`\u001b]0;` + oversized, `\u0007`, `SUFFIX-END-6N2`}},
		{"ST", []string{`\u001b]0;` + oversized, `\u001b\\`, `SUFFIX-END-5M1`}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			m := answerThread(t)
			for _, delta := range tc.fragments {
				m = rawUpdate(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"` + delta + `"}`)})
			}
			item := m.liveItem("a1")
			suffix := tc.fragments[len(tc.fragments)-1]
			if got := string(item.text); !strings.Contains(got, suffix) || strings.Contains(got, "ZZZZ") {
				t.Fatalf("text after fragment-end terminator = %q, want the suffix without the payload", got)
			}
			if got := item.tail(); !strings.Contains(got, suffix) {
				t.Fatalf("live tail after fragment-end terminator = %q, want the suffix", got)
			}
		})
	}
}

func TestAnswerPendingStaysBoundedAfterHugeCompleteFragment(t *testing.T) {
	t.Parallel()
	m := answerThread(t)
	visible := strings.Repeat("complete line of text\n", 10000)
	m = rawUpdate(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(fmt.Sprintf(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":%q}`, visible))})
	item := m.liveItem("a1")
	if got := len(item.pending); got > pendingLimit {
		t.Fatalf("pending after huge visible fragment is %d bytes, want at most %d", got, pendingLimit)
	}
	if got := cap(item.pending); got > pendingLimit {
		t.Fatalf("pending capacity after huge visible fragment is %d bytes, want at most %d", got, pendingLimit)
	}
	if got := string(item.text); got != visible {
		t.Fatalf("huge visible fragment kept %d bytes, want %d", len(got), len(visible))
	}
	controls := strings.Repeat(`\u001b[31mX\u001b[0m\n`, 20000)
	m = rawUpdate(t, m, notificationMsg{method: "item/agentMessage/delta", params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"a1","delta":"` + controls + `"}`)})
	item = m.liveItem("a1")
	if got := len(item.pending); got > pendingLimit {
		t.Fatalf("pending after huge control fragment is %d bytes, want at most %d", got, pendingLimit)
	}
	if got := cap(item.pending); got > pendingLimit {
		t.Fatalf("pending capacity after huge control fragment is %d bytes, want at most %d", got, pendingLimit)
	}
	want := visible + strings.Repeat("X\n", 20000)
	if got := string(item.text); got != want {
		t.Fatalf("text after huge control fragment is %d bytes, want %d", len(got), len(want))
	}
	if got := m.liveItem("a1").tail(); !strings.Contains(got, "X") {
		t.Fatalf("live tail after huge control fragment = %q, want visible text", got)
	}
}

func TestBlockSplitterDiagramFenceBuffers(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, tail := feedAll(&s, "```mermaid\ngraph LR\n a-->b\n")
	if final != nil || tail != "" {
		t.Fatalf("diagram fence buffers: final %#v tail %q", final, tail)
	}
	final, _ = feedAll(&s, "```\n")
	want := []block{{text: "graph LR\n a-->b", code: true, diagram: "mermaid"}}
	if !reflect.DeepEqual(final, want) {
		t.Fatalf("diagram close: %#v", final)
	}
}

func TestBlockSplitterUnclosedDiagramFenceShowsSource(t *testing.T) {
	t.Parallel()
	var s blockSplitter
	final, _ := feedAll(&s, "```d2\nx -> y\n")
	got := s.Close()
	want := []block{{text: "```d2", code: true}, {text: "x -> y", code: true}}
	if !reflect.DeepEqual(append(final, got...), want) {
		t.Fatalf("unclosed diagram fence: %#v", append(final, got...))
	}
}
