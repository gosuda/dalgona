package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestChipDraftBlocksEmptyComposerActions(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		key  tea.KeyPressMsg
		add  string
	}{
		{name: "dequeue", key: press(tea.KeyUp, tea.ModAlt, "")},
		{name: "rewind"},
		{name: "help", key: press('?', 0, "?"), add: "?"},
		{name: "shell", key: press('!', 0, "!"), add: "!"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := readyModel(t, new(fakeCaller))
			pasted := pasteLines(10)
			m.input.composer.paste(pasted)
			m.queue = []followup{{text: "queued"}}
			if tc.name == "rewind" {
				m.applyRewind("rewound")
			} else {
				m, _ = drive(t, m, tc.key)
			}
			if m.input.composer.Expanded() != pasted+tc.add || len(m.queue) != 1 || m.help != nil || m.input.mode != inputPrompt {
				t.Fatalf("%s changed the chip draft: draft %q, queue %d, help %t, mode %v", tc.name, m.input.composer.Expanded(), len(m.queue), m.help != nil, m.input.mode)
			}
		})
	}
}

func TestComposerRepairsCaretAfterClustersMerge(t *testing.T) {
	t.Parallel()
	for _, action := range []string{"insert", "delete separator", "delete chip"} {
		t.Run(action, func(t *testing.T) {
			t.Parallel()
			e := newEditor()
			switch action {
			case "insert":
				e.SetValue("\u0301")
				e.cursor = 0
				e.insert([]rune("e"))
			case "delete separator":
				e.SetValue("e\n\u0301")
				e.cursor = 1
				e.delete()
			case "delete chip":
				e.SetValue("\u0301")
				e.cursor = 0
				e.paste(pasteLines(10))
				e.back(editor.previousStop)
				e.insert([]rune("e"))
				e.forward(editor.nextStop)
				e.backspace()
			}
			if e.Value() != "e\u0301" || e.cursor != 2 {
				t.Fatalf("got %q at rune %d; want caret after combined cluster", e.Value(), e.cursor)
			}
			e.backspace()
			if got := e.Value(); got != "" {
				t.Fatalf("backspace left %q; want the entire cluster removed", got)
			}
		})
	}
}

func TestOversizedEncodedPromptLeavesDraftAndStateUnchanged(t *testing.T) {
	t.Parallel()
	for _, action := range []string{"send", "steer", "queue"} {
		t.Run(action, func(t *testing.T) {
			t.Parallel()
			assertOversizedPrompt(t, action)
		})
	}
}

func assertOversizedPrompt(t *testing.T, action string) {
	t.Helper()
	caller := new(fakeCaller)
	m := readyModel(t, caller)
	if action != "send" {
		m.thread.state = stateRunning
		m.thread.turnID = "t-existing"
	}
	pasted := "visible\n\n\n\n\n\n\n\n\n" + strings.Repeat("\x01", 1<<20-32)
	for range 3 {
		m.input.composer.paste(pasted)
	}
	before := snapshotPrompt(m)
	submit := m.enterPrompt
	if action == "queue" {
		submit = m.queueFollowup
	}
	cmd := submit()
	if after := snapshotPrompt(m); after != before || m.notice == "" {
		t.Fatalf("oversized input changed draft or turn state: caret %d:%d, history %d, queue %d, state %q", after.runeOffset, after.chipSlot, after.history, after.queued, after.state)
	}
	if cmd != nil || len(caller.calls) != 0 {
		t.Fatal("oversized input scheduled an RPC call")
	}
}

func TestOversizedRPCEnvelopeLeavesPromptIntact(t *testing.T) {
	t.Parallel()
	m := readyModel(t, new(fakeCaller))
	m.input.composer.SetValue("small prompt")
	m.thread.modelName = strings.Repeat("\x01", 3<<20)
	before := snapshotPrompt(m)
	if cmd := m.submitPrompt(); cmd != nil || snapshotPrompt(m) != before || m.notice == "" {
		t.Fatal("oversized RPC metadata did not preserve the prompt and turn state")
	}
}

type promptSnapshot struct {
	text, state                           string
	runeOffset, chipSlot, history, queued int
}

func snapshotPrompt(m model) promptSnapshot {
	e := m.input.composer
	return promptSnapshot{text: e.Expanded(), state: m.thread.state, runeOffset: e.cursor, chipSlot: e.lead, history: len(m.input.history.entries), queued: len(m.queue)}
}

func BenchmarkComposerWordNavigation(b *testing.B) {
	b.Run("2048 pictographs", func(b *testing.B) { benchmarkComposerWordNavigation(b, 2048) })
	b.Run("8192 pictographs", func(b *testing.B) { benchmarkComposerWordNavigation(b, 8192) })
}

func benchmarkComposerWordNavigation(b *testing.B, size int) {
	e := newEditor()
	e.SetValue(strings.Repeat("😀", size))
	for b.Loop() {
		e.back(editor.wordStart)
		if e.cursor != 0 {
			b.Fatalf("backward caret = %d; want 0", e.cursor)
		}
		e.forward(editor.wordEnd)
		if e.cursor != size {
			b.Fatalf("forward caret = %d; want %d", e.cursor, size)
		}
	}
}

func TestVerticalCaretUsesPinnedGraphemeStops(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{})
	m, _ = drive(t, m, tea.ModeReportMsg{Mode: ansi.ModeUnicodeCore, Value: ansi.ModeSet})
	m.input.composer.SetValue("x\u094d\u0915\nz")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	m, _ = drive(t, m, press('Q', 0, "Q"))
	if got := m.input.composer.Value(); got != "Qx\u094d\u0915\nz" {
		t.Fatalf("insertion after vertical move left %q; want insertion before the whole cluster", got)
	}
}

func TestVerticalCaretMovesPastAdjacentChip(t *testing.T) {
	t.Parallel()
	m := readyModel(t, new(fakeCaller))
	m.input.composer.SetValue("first\nab")
	m.input.composer.paste(pasteLines(10))
	m = pressKeys(t, m, press(tea.KeyUp, 0, ""), press(tea.KeyUp, 0, ""), press('X', 0, "X"))
	if got := m.input.composer.Value(); got != "fiXrst\nab" {
		t.Fatalf("Up got stuck on the chip boundary: %q; want typing on the first line", got)
	}
}

func TestSlashGrammarRejectsUnicodeDelimiter(t *testing.T) {
	t.Parallel()
	m := readyModel(t, new(fakeCaller))
	const draft = "/model\u00a0model-id"
	m.input.composer.SetValue(draft)
	if cmd := m.submitPrompt(); cmd != nil || m.input.composer.Value() != draft {
		t.Fatal("Unicode whitespace authorized a command or discarded the draft")
	}
}

func TestSlashGrammarPreservesArguments(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := readyModel(t, caller)
	const arg = "\u00a0model-id\u00a0"
	m.input.composer.SetValue("/model " + arg)
	runCommand(t, m, m.submitPrompt(), "ASCII delimiter did not authorize /model")
	if len(caller.calls) != 1 {
		t.Fatalf("got %d calls; want one config write", len(caller.calls))
	}
	got, ok := caller.calls[0].params.(rpc.ConfigValueWriteParams)
	if !ok || got.Value != arg {
		t.Fatalf("call = %#v; want exact argument %q", caller.calls[0], arg)
	}
}

func TestFormattingScalarDraftSendsAndSaves(t *testing.T) {
	t.Parallel()
	const text = "\ufeff"
	t.Run("send", func(t *testing.T) {
		t.Parallel()
		caller := new(fakeCaller)
		m := readyModel(t, caller)
		m.queue = []followup{{text: "held"}}
		m.input.composer.SetValue(text)
		runCommand(t, m, m.submitPrompt(), "formatting-only draft was not sent")
		assertTurnStartText(t, caller, text)
	})
	t.Run("save", func(t *testing.T) {
		t.Parallel()
		m := readyModel(t, new(fakeCaller))
		m.input.composer.SetValue(text)
		m.exitKey()
		if got := historyTexts(m); len(got) != 1 || got[0] != text {
			t.Fatalf("saved drafts = %q; want %q", got, text)
		}
	})
}
