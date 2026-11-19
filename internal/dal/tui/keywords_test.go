package tui

import (
	"context"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/colorprofile"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type failingKeywordCaller struct {
	fakeCaller
}

func (f *failingKeywordCaller) Call(ctx context.Context, method string, params, result any) error {
	if method == "dalgona/keywords/match" {
		return &rpc.RPCError{Code: -32601, Message: "method not found"}
	}
	return f.fakeCaller.Call(ctx, method, params, result)
}

func keywordReplyMsg(seq int, text string, ranges ...[2]int) resultMsg {
	reply := new(keywordReply)
	for _, r := range ranges {
		reply.Matches = append(reply.Matches, keywordReplyMatch{ByteRange: keywordRange{Start: r[0], End: r[1]}})
	}
	return resultMsg{op: "keywordMatch", value: reply, seq: seq, text: text}
}

func underlinedRunes(text string) string {
	var out strings.Builder
	under, last := false, 0
	for _, loc := range sgrPattern.FindAllStringSubmatchIndex(text, -1) {
		if under {
			out.WriteString(text[last:loc[0]])
		}
		under = underlineAfter(under, text[loc[2]:loc[3]])
		last = loc[1]
	}
	if under {
		out.WriteString(text[last:])
	}
	return out.String()
}

func underlineAfter(under bool, params string) bool {
	for param := range strings.SplitSeq(params, ";") {
		switch param {
		case "4":
			under = true
		case "", "0", "24":
			under = false
		}
	}
	return under
}

func TestKeywordMarkUnderlinedUnderNoColor(t *testing.T) {
	t.Parallel()
	text := "use ultrathink now"
	spans := []keywordSpan{{start: 4, end: 14}}
	for _, tc := range []struct {
		name  string
		theme theme
		open  string
	}{
		{name: "before the theme is known", theme: theme{}, open: "\x1b[1m"},
		{name: "known theme without colour", theme: newTheme(themeNight, false), open: "\x1b[1;4m"},
	} {
		out := markKeywords(text, spans, tc.theme)
		if segment := tc.open + "ultrathink" + reset; !strings.Contains(out, segment) {
			t.Fatalf("%s: mark is %q, want %q", tc.name, out, segment)
		}
		if got := stripANSI(out); got != text {
			t.Fatalf("mark changed visible text to %q", got)
		}
	}
}

func TestComposerAndSentMessageKeywordGlow(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.theme = newTheme(themeNight, false)
	m.screen.width = 8
	m.input.composer.SetWidth(8)
	m.input.composer.SetValue("ultrathink")
	m.input.marks = []keywordSpan{{start: 0, end: 10}}
	m.input.marksFor = "ultrathink"
	view := m.View().Content
	if got := strings.ReplaceAll(underlinedRunes(view), "\n", ""); got != "ultrathink" {
		t.Fatalf("wrapped composer lost its keyword mark: %q\n%q", got, view)
	}
	m.screen.width = 40
	settled := settledText(&m, rpc.Item{
		Type:     "userMessage",
		Text:     "ultrathink, please",
		Keywords: []rpc.KeywordMatch{{ByteRange: rpc.ByteRange{Start: 0, End: 10}}},
	})
	if got := underlinedRunes(settled); got != "ultrathink" {
		t.Fatalf("settled user message lost its keyword mark: %q", settled)
	}
}

func TestComposerMarksComeFromServerReply(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.theme = newTheme(themeNight, false)
	m.input.composer.SetValue("please ultrathink")
	if got := underlinedRunes(m.View().Content); got != "" {
		t.Fatalf("composer marked %q before any reply", got)
	}
	_ = m.matchKeywords()
	m, _ = drive(t, m, keywordReplyMsg(m.matchSeq, "please ultrathink", [2]int{7, 17}))
	if got := underlinedRunes(m.View().Content); got != "ultrathink" {
		t.Fatalf("server reply marked %q, want ultrathink", got)
	}
}

func TestStaleKeywordReplyIsDropped(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.theme = newTheme(themeNight, false)
	m.input.composer.SetValue("x ultrathink")
	_ = m.matchKeywords()
	first := m.matchSeq
	m.input.composer.SetValue("ultrathink")
	_ = m.matchKeywords()
	second := m.matchSeq
	m, _ = drive(t, m, keywordReplyMsg(second, "ultrathink", [2]int{0, 10}))
	m, _ = drive(t, m, keywordReplyMsg(first, "x ultrathink", [2]int{2, 12}))
	if got := underlinedRunes(m.View().Content); got != "ultrathink" {
		t.Fatalf("stale reply changed the marks: %q", got)
	}

	fresh := newTestModel(nil, Options{})
	fresh.input.composer.SetValue("ultrathink")
	_ = fresh.matchKeywords()
	fresh, _ = drive(t, fresh, keywordReplyMsg(first, "x ultrathink", [2]int{2, 12}))
	if got := underlinedRunes(fresh.View().Content); got != "" {
		t.Fatalf("stale reply marked %q on a new draft", got)
	}
}

func runCommands(t *testing.T, m model, command tea.Cmd) (model, []tea.Msg) {
	t.Helper()
	var seen []tea.Msg
	for pending := []tea.Cmd{command}; len(pending) > 0; pending = pending[1:] {
		for _, msg := range commandMsgs(pending[0]) {
			seen = append(seen, msg)
			var follow tea.Cmd
			m, follow = updateWithCommand(t, m, msg)
			pending = append(pending, follow)
		}
	}
	return m, seen
}

func TestKeywordMatchErrorIsSilent(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		m := newTestModel(&failingKeywordCaller{}, Options{})
		m.input.composer.SetValue("ultrathink")
		command := m.matchKeywords()
		m, msgs := runCommands(t, m, command)
		failed := false
		for _, msg := range msgs {
			if reply, ok := msg.(resultMsg); ok && reply.op == "keywordMatch" && reply.err != nil {
				failed = true
			}
		}
		if !failed {
			t.Fatal("the failing server was never asked for a match")
		}
		if m.notice != "" || m.noticeError {
			t.Fatalf("a server without keyword matching raised a notice: %q", m.notice)
		}
	})
}

type keywordRecorder struct {
	mu     sync.Mutex
	sent   []keywordMatchParams
	answer func(call int, text string) keywordReply
}

func (k *keywordRecorder) Notify(string, any) error { return nil }

func (k *keywordRecorder) Call(_ context.Context, method string, params, result any) error {
	if method != "dalgona/keywords/match" {
		return nil
	}
	sent := params.(keywordMatchParams)
	k.mu.Lock()
	k.sent = append(k.sent, sent)
	call := len(k.sent)
	k.mu.Unlock()
	if k.answer != nil {
		*result.(*keywordReply) = k.answer(call, sent.Text)
	}
	return nil
}

func (k *keywordRecorder) matches() []keywordMatchParams {
	k.mu.Lock()
	defer k.mu.Unlock()
	return append([]keywordMatchParams(nil), k.sent...)
}

func typeBurst(run *emulatedRun, text string) {
	for _, r := range text {
		run.send(press(r, 0, string(r)))
		time.Sleep(5 * time.Millisecond)
	}
	synctest.Wait()
}

func pause(d time.Duration) {
	time.Sleep(d)
	synctest.Wait()
}

func expectMatches(t *testing.T, client *keywordRecorder, when string, want ...keywordMatchParams) {
	t.Helper()
	if got := client.matches(); !slices.Equal(got, want) {
		t.Fatalf("%s: matches = %+v, want %+v", when, got, want)
	}
}

func TestKeywordMatchSentOncePerPause(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name  string
		state string
		kind  string
	}{
		{name: "no turn runs", state: stateIdle, kind: "prompt"},
		{name: "a turn runs", state: stateRunning, kind: "steer"},
		{name: "a turn waits for approval", state: stateAwaitingApproval, kind: "steer"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				client := new(keywordRecorder)
				run := startRecordedWith(client, 100, 24, new(recordedOutput))
				defer run.stop()
				run.send(func(m *model) { m.thread.id, m.thread.state = "t1", tc.state })
				synctest.Wait()
				typeBurst(run, "ultrathink")
				expectMatches(t, client, "while ten keys were still arriving")
				pause(114 * time.Millisecond)
				expectMatches(t, client, "119 ms after the last key")
				pause(time.Millisecond)
				first := keywordMatchParams{ThreadID: "t1", Text: "ultrathink", Kind: tc.kind}
				expectMatches(t, client, "120 ms after the last key", first)
				typeBurst(run, "!")
				pause(time.Second)
				second := keywordMatchParams{ThreadID: "t1", Text: "ultrathink!", Kind: tc.kind}
				expectMatches(t, client, "after the next pause", first, second)
			})
		})
	}
}

func wordReply(text string, width int) keywordReply {
	start := strings.Index(text, "ultrathink")
	return keywordReply{Matches: []keywordReplyMatch{{ByteRange: keywordRange{Start: start, End: start + width}}}}
}

func composerMarks(run *emulatedRun) (spans []keywordSpan, shown string) {
	spans = run.read(func(m model) any { return m.draftSpans() }).([]keywordSpan)
	shown = run.read(func(m model) any {
		m = playTerminal(m)
		m.screen.profile = colorprofile.TrueColor
		return underlinedRunes(m.View().Content)
	}).(string)
	return spans, shown
}

func TestKeywordReplyAfterPauseMarksTheDraft(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		client := &keywordRecorder{answer: func(_ int, text string) keywordReply { return wordReply(text, len("ultrathink")) }}
		run := startRecordedWith(client, 100, 24, new(recordedOutput))
		defer run.stop()
		run.send(func(m *model) { m.screen.theme = newTheme(themeNight, false) })
		synctest.Wait()
		typeBurst(run, "use ultrathink")
		if spans, shown := composerMarks(run); len(spans) != 0 || shown != "" {
			t.Fatalf("marks %v %q before the pause ended, want none", spans, shown)
		}
		pause(time.Second)
		spans, shown := composerMarks(run)
		if want := []keywordSpan{{start: 4, end: 14}}; !slices.Equal(spans, want) {
			t.Fatalf("spans after the pause = %v, want %v", spans, want)
		}
		if shown != "ultrathink" {
			t.Fatalf("composer underlines %q after the pause, want ultrathink", shown)
		}
	})
}

func TestKeywordReplyOfAnEarlierRequestIsDropped(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		held := make(chan struct{})
		release := sync.OnceFunc(func() { close(held) })
		client := &keywordRecorder{answer: func(call int, text string) keywordReply {
			if call == 1 {
				<-held
				return wordReply(text, len("ultra"))
			}
			return wordReply(text, len("ultrathink"))
		}}
		run := startRecordedWith(client, 100, 24, new(recordedOutput))
		defer run.stop()
		defer release()
		typeBurst(run, "ultrathink")
		pause(time.Second)
		typeBurst(run, "!")
		run.send(press(tea.KeyBackspace, 0, ""))
		pause(time.Second)
		if spans, _ := composerMarks(run); !slices.Equal(spans, []keywordSpan{{start: 0, end: 10}}) {
			t.Fatalf("spans from the newest request = %v, want [{0 10}]", spans)
		}
		release()
		synctest.Wait()
		if spans, _ := composerMarks(run); !slices.Equal(spans, []keywordSpan{{start: 0, end: 10}}) {
			t.Fatalf("the reply of the first request replaced the marks of the newest one: %v", spans)
		}
		if got := len(client.matches()); got != 2 {
			t.Fatalf("the draft was sent %d times, want 2", got)
		}
	})
}

func TestKeywordMatchSkipsLargeDraft(t *testing.T) {
	t.Parallel()
	const limit = 64 << 10
	for _, tc := range []struct {
		name  string
		bytes int
		sent  int
	}{
		{name: "64 KiB", bytes: limit, sent: 1},
		{name: "over 64 KiB", bytes: limit + 1, sent: 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			synctest.Test(t, func(t *testing.T) {
				client := new(keywordRecorder)
				run := startRecordedWith(client, 100, 24, new(recordedOutput))
				defer run.stop()
				run.send(func(m *model) { m.input.composer.SetValue(strings.Repeat("x", tc.bytes-1)) })
				synctest.Wait()
				typeBurst(run, "x")
				pause(time.Second)
				if got := client.matches(); len(got) != tc.sent {
					t.Fatalf("a draft of %d bytes sent %d matches, want %d", tc.bytes, len(got), tc.sent)
				}
			})
		})
	}
}

func TestComposerMarkWithCursorInsideWord(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m.screen.theme = newTheme(themeNight, false)
	m.input.composer.SetValue("ultrathink")
	m.input.composer.cursor = 5
	m.input.marks = []keywordSpan{{start: 0, end: 10}}
	m.input.marksFor = "ultrathink"
	view := m.View().Content
	if got := stripANSI(view); !strings.Contains(got, "› ultrathink") {
		t.Fatalf("cursor inside the word broke the composer: %q", got)
	}
	if cursor := m.View().Cursor; cursor == nil || cursor.X != 7 {
		t.Fatalf("terminal cursor is not after the fifth letter: %+v", cursor)
	}
	if got := underlinedRunes(view); got != "ultrathink" {
		t.Fatalf("mark does not cover both parts around the cursor: %q", got)
	}
}

func stripANSI(text string) string {
	var out strings.Builder
	for i := 0; i < len(text); {
		if text[i] != '\x1b' {
			out.WriteByte(text[i])
			i++
			continue
		}
		i += 2
		for i < len(text) && text[i] != 'm' {
			i++
		}
		if i < len(text) {
			i++
		}
	}
	return out.String()
}
