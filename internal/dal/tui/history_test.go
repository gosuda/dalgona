package tui

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func typeText(t *testing.T, m model, text string) model {
	t.Helper()
	for _, r := range text {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	return m
}

func TestHistoryRecallAndDraftRestore(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "a")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = typeText(t, m, "dr")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	if got := m.input.composer.Value(); got != "a" {
		t.Fatalf("up recalled %q, want a", got)
	}
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Value(); got != "dr" {
		t.Fatalf("down restored %q, want dr", got)
	}
}

func TestLoginLineNotInHistory(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "//login openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if n := len(m.input.history.entries); n != 0 {
		t.Fatalf("history holds %d entries after //login, want 0", n)
	}
	var h history
	h.add("/login x", "")
	if len(h.entries) != 0 {
		t.Fatalf("direct add of /login kept %d entries, want 0", len(h.entries))
	}
}

type numberedTurns struct {
	fakeCaller
	started int
}

func (c *numberedTurns) Call(ctx context.Context, method string, params, result any) error {
	out, ok := result.(*rpc.TurnStartResponse)
	if !ok {
		return c.fakeCaller.Call(ctx, method, params, result)
	}
	c.started++
	c.calls = append(c.calls, callRecord{method: method, params: params})
	out.Turn = rpc.Turn{ID: fmt.Sprintf("turn-%d", c.started), Status: "inProgress"}
	return nil
}

func TestResentPromptTakesNewTurn(t *testing.T) {
	t.Parallel()
	m := newModel(new(numberedTurns), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "hi")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, command())
	m, _ = drive(t, m, notificationMsg{method: "turn/completed", params: []byte(`{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`)})
	m = typeText(t, m, "hi")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, command())
	if entries := m.input.history.entries; len(entries) != 1 || entries[0].turnID != "turn-2" {
		t.Fatalf("history = %#v, want one entry for turn-2", entries)
	}
}

func TestSkippedRepeatTakesGivenTurn(t *testing.T) {
	t.Parallel()
	var h history
	h.add("hi", "turn-1")
	h.add("hi", "turn-2")
	if len(h.entries) != 1 || h.entries[0].turnID != "turn-2" {
		t.Fatalf("entries = %#v, want one entry for turn-2", h.entries)
	}
}

func TestCtrlCSavesDraft(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "dr")
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if got := m.input.composer.Value(); got != "" {
		t.Fatalf("editor holds %q after Ctrl+C, want empty", got)
	}
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].text != "dr" {
		t.Fatalf("history = %#v, want the draft dr", m.input.history.entries)
	}
	if m.hint.text != "Draft saved to history · Ctrl+C again to exit" {
		t.Fatalf("hint = %q", m.hint.text)
	}
}

func hasPrivateUse(text string) bool {
	return strings.ContainsFunc(text, func(r rune) bool { return r >= chipRuneBase })
}

func chipDraftModel(t *testing.T, client caller, pasted string) model {
	t.Helper()
	m := newTestModel(client, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	m = typeText(t, m, "S7MARK ")
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	if len(m.input.composer.chips) != 1 {
		t.Fatalf("composer holds %d chips after the paste, want 1", len(m.input.composer.chips))
	}
	return m
}

func TestCtrlCSavesChipDraftExpanded(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(30)
	want := "S7MARK " + pasted
	m := chipDraftModel(t, new(fakeCaller), pasted)
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if got := m.input.composer.Value(); got != "" {
		t.Fatalf("editor holds %q after Ctrl+C, want empty", got)
	}
	if m.hint.text != "Draft saved to history · Ctrl+C again to exit" {
		t.Fatalf("hint = %q", m.hint.text)
	}
	if got := historyTexts(m); !slices.Equal(got, []string{want}) {
		t.Fatalf("history = %q, want the typed text and all 30 pasted lines", got)
	}
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	if got := m.input.composer.Expanded(); got != want {
		t.Fatalf("recalled draft = %q, want %q", got, want)
	}
	if hasPrivateUse(m.input.composer.Value()) {
		t.Fatalf("recalled draft holds a chip atom: %q", m.input.composer.Value())
	}
}

func TestRecalledChipDraftSendsAllPastedLines(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(30)
	caller := new(fakeCaller)
	m := chipDraftModel(t, caller, pasted)
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the recalled draft")
	if len(caller.calls) == 0 || caller.calls[0].method != "turn/start" {
		t.Fatalf("calls = %#v, want turn/start", caller.calls)
	}
	input := caller.calls[0].params.(rpc.TurnStartParams).Input
	if want := "S7MARK " + pasted; len(input) != 1 || input[0].Text != want {
		t.Fatalf("sent %q, want the typed text and all 30 pasted lines", input)
	}
}

var slashPastes = []struct{ name, text string }{
	{name: "double slash", text: "// Render the menu\n" + pasteLines(11)},
	{name: "command with a word", text: "/new now\n" + pasteLines(11)},
	{name: "bare command", text: "/new\n" + pasteLines(11)},
}

var historyRecalls = []struct {
	name string
	run  func(*testing.T, model) model
}{
	{name: "arrow", run: func(t *testing.T, m model) model {
		t.Helper()
		m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
		return m
	}},
	{name: "search", run: func(t *testing.T, m model) model {
		t.Helper()
		m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
		if m.list == nil || m.list.kind != "history" {
			t.Fatal("ctrl+r did not open the history panel")
		}
		m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
		return m
	}},
}

func pastedDraftModel(t *testing.T, caller caller, pasted string) model {
	t.Helper()
	m := newTestModel(caller, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	if len(m.input.composer.chips) != 1 {
		t.Fatalf("composer holds %d chips after the paste, want 1", len(m.input.composer.chips))
	}
	return m
}

func assertTurnStartText(t *testing.T, caller *fakeCaller, want string) {
	t.Helper()
	if len(caller.calls) != 1 || caller.calls[0].method != "turn/start" {
		t.Fatalf("calls = %#v, want one turn/start", caller.calls)
	}
	input := caller.calls[0].params.(rpc.TurnStartParams).Input
	if len(input) != 1 || input[0].Text != want {
		t.Fatalf("sent %q, want the pasted text unchanged %q", input, want)
	}
}

func TestRecalledChipDraftIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, recall := range historyRecalls {
			t.Run(paste.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := pastedDraftModel(t, caller, paste.text)
				m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
				m = recall.run(t, m)
				if got := m.input.composer.Expanded(); got != paste.text {
					t.Fatalf("recalled draft = %q, want the pasted text", got)
				}
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the recalled draft; notice: "+m.notice)
				assertTurnStartText(t, caller, paste.text)
			})
		}
	}
}

func TestSentChipDraftRecalledIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, recall := range historyRecalls {
			t.Run(paste.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := pastedDraftModel(t, caller, paste.text)
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				m = runCommand(t, m, command, "Enter did not send the chip draft")
				assertTurnStartText(t, caller, paste.text)
				echo := fmt.Appendf(nil, `{"threadId":"t1","item":{"id":"u1","type":"userMessage","text":%q}}`, paste.text)
				m, _ = drive(t, m, notificationMsg{method: "item/completed", params: echo})
				if n := len(m.input.history.entries); n != 1 {
					t.Fatalf("history holds %d entries after the echo, want 1", n)
				}
				m = recall.run(t, m)
				m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the recalled prompt; notice: "+m.notice)
				last := caller.calls[len(caller.calls)-1]
				steer, ok := last.params.(rpc.TurnSteerParams)
				if last.method != "turn/steer" || !ok || len(steer.Input) != 1 || steer.Input[0].Text != paste.text {
					t.Fatalf("last call = %s %#v, want turn/steer with the pasted text unchanged", last.method, last.params)
				}
			})
		}
	}
}

var recalledEdits = []struct {
	name   string
	keys   []tea.KeyPressMsg
	suffix string
}{
	{name: "typed character", keys: []tea.KeyPressMsg{press('x', 0, "x")}, suffix: "x"},
	{name: "typed and deleted character", keys: []tea.KeyPressMsg{press('x', 0, "x"), press(tea.KeyBackspace, 0, "")}},
}

func editedRecalledModel(t *testing.T, caller *fakeCaller, pasted string, recall func(*testing.T, model) model, keys []tea.KeyPressMsg) model {
	t.Helper()
	m := pastedDraftModel(t, caller, pasted)
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	m = recall(t, m)
	for _, key := range keys {
		m, _ = drive(t, m, key)
	}
	return m
}

func TestEditedRecalledChipDraftIsParsedAsTypedText(t *testing.T) {
	t.Parallel()
	pasted := slashPastes[0].text
	for _, edit := range recalledEdits {
		for _, recall := range historyRecalls {
			t.Run(edit.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := editedRecalledModel(t, caller, pasted, recall.run, edit.keys)
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the edited draft")
				assertTurnStartText(t, caller, strings.TrimPrefix(pasted, "/")+edit.suffix)
			})
		}
	}
}

func TestEditedRecalledChipDraftRunsTheTypedCommand(t *testing.T) {
	t.Parallel()
	for _, edit := range recalledEdits {
		for _, recall := range historyRecalls {
			t.Run(edit.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := editedRecalledModel(t, caller, slashPastes[1].text, recall.run, edit.keys)
				m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
				if len(caller.calls) != 0 || m.confirm == nil || m.confirm.kind != confirmNew {
					t.Fatalf("calls = %#v, confirm = %#v, want the new-thread confirmation of the typed /new and no request", caller.calls, m.confirm)
				}
			})
		}
	}
}

func TestRepeatedCtrlCSaveTakesTheIntentOfTheEditedDraft(t *testing.T) {
	t.Parallel()
	for _, recall := range historyRecalls {
		t.Run(recall.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := editedRecalledModel(t, caller, slashPastes[1].text, recall.run, recalledEdits[1].keys)
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			if n := len(m.input.history.entries); n != 1 {
				t.Fatalf("history holds %d entries after the second save, want 1", n)
			}
			m = recall.run(t, m)
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			if command != nil {
				m = runCommand(t, m, command, "")
			}
			if len(caller.calls) != 0 || m.confirm == nil || m.confirm.kind != confirmNew {
				t.Fatalf("calls = %#v, confirm = %#v, want the new-thread confirmation of the typed /new and no request", caller.calls, m.confirm)
			}
		})
	}
}

func TestTypedCommandDraftSavedTwiceStaysACommand(t *testing.T) {
	t.Parallel()
	for _, recall := range historyRecalls {
		t.Run(recall.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := newTestModel(caller, Options{})
			m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
			m.thread.id = "t1"
			for range 2 {
				m = typeText(t, m, "/new now")
				m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			}
			if got := m.input.history.entries; len(got) != 1 || got[0].verbatim {
				t.Fatalf("entries = %#v, want one entry with no paste intent", got)
			}
			m = recall.run(t, m)
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			if len(caller.calls) != 0 || m.confirm == nil || m.confirm.kind != confirmNew {
				t.Fatalf("calls = %#v, confirm = %#v, want the new-thread confirmation and no request", caller.calls, m.confirm)
			}
		})
	}
}

func TestChipDraftSavedTwiceStaysSentUnchanged(t *testing.T) {
	t.Parallel()
	for _, recall := range historyRecalls {
		t.Run(recall.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := pastedDraftModel(t, caller, slashPastes[1].text)
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			m, _ = drive(t, m, tea.PasteMsg{Content: slashPastes[1].text})
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			if got := m.input.history.entries; len(got) != 1 || !got[0].verbatim {
				t.Fatalf("entries = %#v, want one entry with the paste intent", got)
			}
			m = recall.run(t, m)
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			runCommand(t, m, command, "Enter did not send the recalled draft; notice: "+m.notice)
			assertTurnStartText(t, caller, slashPastes[1].text)
		})
	}
}

func TestHistoryRepeatSetsTheIntentAndASavedDraftReplacesIt(t *testing.T) {
	t.Parallel()
	const pasted = "/new now\nb"
	chipFirst := editor{text: []rune{chipAtom(0)}, chips: []chip{{label: "[Paste #1 +2 lines]", text: pasted}}}
	typed := editor{text: []rune(pasted)}
	tests := []struct {
		name   string
		first  editor
		repeat func(*history)
		want   bool
	}{
		{name: "echo keeps the intent", first: chipFirst, repeat: func(h *history) { h.add(pasted, "turn-1") }, want: true},
		{name: "echo sets the intent", first: typed, repeat: func(h *history) { h.add(pasted, "turn-1") }, want: true},
		{name: "saved draft clears the intent", first: chipFirst, repeat: func(h *history) { h.saveDraft(typed, pasted) }, want: false},
		{name: "saved draft sets the intent", first: typed, repeat: func(h *history) { h.saveDraft(chipFirst, pasted) }, want: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			var h history
			h.saveDraft(test.first, pasted)
			test.repeat(&h)
			if len(h.entries) != 1 || h.entries[0].verbatim != test.want {
				t.Fatalf("entries = %#v, want one entry with verbatim %v", h.entries, test.want)
			}
		})
	}
}

func sentPromptsModel(t *testing.T, prompts ...string) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	for _, prompt := range prompts {
		var command tea.Cmd
		m = typeText(t, m, prompt)
		m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
		m = runCommand(t, m, command, "Enter did not send "+prompt)
		m.thread.state = stateIdle
	}
	return m
}

func TestUpAfterARepeatedPromptRecallsTheNewestEntry(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name    string
		prompts []string
		repeat  func(*testing.T, model) model
	}{
		{name: "saved by Ctrl+C", prompts: []string{"first", "second"}, repeat: func(t *testing.T, m model) model {
			t.Helper()
			m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			return m
		}},
		{name: "saved by Ctrl+C from a single entry", prompts: []string{"second"}, repeat: func(t *testing.T, m model) model {
			t.Helper()
			m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
			m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
			return m
		}},
		{name: "sent again", prompts: []string{"first", "second"}, repeat: func(t *testing.T, m model) model {
			t.Helper()
			m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			m = runCommand(t, m, command, "Enter did not send the recalled prompt")
			m.thread.state = stateIdle
			return m
		}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			m := test.repeat(t, sentPromptsModel(t, test.prompts...))
			if n := len(m.input.history.entries); n != len(test.prompts) {
				t.Fatalf("history holds %d entries after the repeat, want %d", n, len(test.prompts))
			}
			m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
			if got, want := m.input.composer.Value(), test.prompts[len(test.prompts)-1]; got != want {
				t.Fatalf("up recalled %q, want the newest entry %q", got, want)
			}
		})
	}
}

func TestEchoOfTheLastPromptKeepsTheRecallPosition(t *testing.T) {
	t.Parallel()
	m := sentPromptsModel(t, "a")
	m = typeText(t, m, "dr")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	m, _ = drive(t, m, userMessageNotification("a"))
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Value(); got != "dr" {
		t.Fatalf("down restored %q after the echo, want the typed draft", got)
	}
}

func TestRecalledChipDraftRestoreKeepsTypedDraftReadingSlash(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := pastedDraftModel(t, caller, slashPastes[0].text)
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	m = typeText(t, m, "//a b")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	if got := m.input.composer.Expanded(); got != slashPastes[0].text {
		t.Fatalf("up recalled %q, want the pasted text", got)
	}
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Value(); got != "//a b" {
		t.Fatalf("down restored %q, want the typed draft", got)
	}
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the restored draft")
	assertTurnStartText(t, caller, "/a b")
}

const completedTurn = `{"threadId":"t1","turn":{"id":"turn-1","items":[],"status":"completed"}}`

func runningChipDraftModel(t *testing.T, caller caller, pasted string) model {
	t.Helper()
	m := pastedDraftModel(t, caller, pasted)
	m.thread.turnID = "turn-1"
	m.thread.state = stateRunning
	return m
}

func completeTurn(t *testing.T, m model) model {
	t.Helper()
	m, command := drive(t, m, notificationMsg{method: "turn/completed", params: []byte(completedTurn)})
	if command != nil {
		m, _ = drive(t, m, command())
	}
	return m
}

func assertLastCallText(t *testing.T, caller *fakeCaller, count int, method, want string) {
	t.Helper()
	for _, call := range caller.calls {
		if call.method == "thread/start" {
			t.Fatalf("calls = %#v, want no thread/start", caller.calls)
		}
	}
	if len(caller.calls) != count {
		t.Fatalf("calls = %#v, want %d requests with the last a %s of the pasted text %q", caller.calls, count, method, want)
	}
	last := caller.calls[len(caller.calls)-1]
	var input []rpc.InputItem
	switch params := last.params.(type) {
	case rpc.TurnStartParams:
		input = params.Input
	case rpc.TurnSteerParams:
		input = params.Input
	}
	if last.method != method || len(input) != 1 || input[0].Text != want {
		t.Fatalf("last call = %s %#v, want %s with the pasted text unchanged %q", last.method, last.params, method, want)
	}
}

func TestQueuedChipFollowupRecalledIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, recall := range historyRecalls {
			t.Run(paste.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := runningChipDraftModel(t, caller, paste.text)
				m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
				m = completeTurn(t, m)
				assertLastCallText(t, caller, 1, "turn/start", paste.text)
				m = completeTurn(t, m)
				m = recall.run(t, m)
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				m = runCommand(t, m, command, "Enter did not send the recalled follow-up")
				if m.confirm != nil {
					t.Fatalf("Enter on the recalled follow-up opened %#v, want the prompt sent", m.confirm)
				}
				assertLastCallText(t, caller, 2, "turn/start", paste.text)
			})
		}
	}
}

func TestTakenBackQueuedChipFollowupIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		t.Run(paste.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := runningChipDraftModel(t, caller, paste.text)
			m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
			if got := m.input.composer.Expanded(); got != paste.text {
				t.Fatalf("alt+up moved %q into the composer, want the pasted text", got)
			}
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			m = runCommand(t, m, command, "Enter did not send the taken-back follow-up")
			if m.confirm != nil {
				t.Fatalf("Enter on the taken-back follow-up opened %#v, want the prompt sent", m.confirm)
			}
			assertLastCallText(t, caller, 1, "turn/steer", paste.text)
		})
	}
}

func TestTakenBackTypedFollowupKeepsTheIntentOfItsDraft(t *testing.T) {
	t.Parallel()
	tests := []struct{ name, typed string }{
		{name: "plain text", typed: "plain text"},
		{name: "typed command", typed: "/new now"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			m := newModel(new(fakeCaller), Options{}, nil)
			m.thread.id = "t1"
			m.thread.state = stateRunning
			m = typeText(t, m, test.typed)
			m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
			assertComposer(t, m.input.composer, test.typed, false)
		})
	}
}

func TestQueuedTypedFollowupIsRecordedAsSentBeforeTheEcho(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name     string
		typed    string
		verbatim bool
	}{
		{name: "plain text", typed: "plain text"},
		{name: "typed command", typed: "/new now", verbatim: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := newModel(caller, Options{}, nil)
			m.thread.id = "t1"
			m.thread.turnID = "turn-1"
			m.thread.state = stateRunning
			m = typeText(t, m, test.typed)
			m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			m = completeTurn(t, m)
			if entries := m.input.history.entries; len(entries) != 1 || entries[0].text != test.typed || entries[0].verbatim != test.verbatim {
				t.Fatalf("history = %#v, want one entry for the follow-up with verbatim %v", entries, test.verbatim)
			}
			m = completeTurn(t, m)
			m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
			assertComposer(t, m.input.composer, test.typed, test.verbatim)
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			m = runCommand(t, m, command, "Enter did not send the recalled follow-up")
			if m.confirm != nil {
				t.Fatalf("Enter on the recalled follow-up opened %#v, want the prompt sent", m.confirm)
			}
			assertLastCallText(t, caller, 2, "turn/start", test.typed)
		})
	}
}

func TestTypedDoubleSlashPromptIsRecalledSentUnchangedBeforeTheEcho(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name   string
		state  string
		method string
	}{
		{name: "send", state: stateIdle, method: "turn/start"},
		{name: "steer", state: stateRunning, method: "turn/steer"},
	}
	for _, test := range tests {
		for _, recall := range historyRecalls {
			t.Run(test.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := newTestModel(caller, Options{})
				m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
				m.thread.id = "t1"
				m.thread.turnID = "turn-1"
				m.thread.state = test.state
				m = typeText(t, m, "//new now")
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				m = runCommand(t, m, command, "Enter did not send the typed double slash prompt")
				m.thread.state = test.state
				if entries := m.input.history.entries; len(entries) != 1 || entries[0].text != "/new now" || !entries[0].verbatim {
					t.Fatalf("history = %#v, want one entry sent unchanged", entries)
				}
				m = recall.run(t, m)
				m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
				m = runCommand(t, m, command, "Enter did not send the recalled prompt; notice: "+m.notice)
				if m.confirm != nil {
					t.Fatalf("Enter on the recalled prompt opened %#v, want the prompt sent", m.confirm)
				}
				assertLastCallText(t, caller, 2, test.method, "/new now")
			})
		}
	}
}

type refusedStarts struct{ fakeCaller }

func (c *refusedStarts) Call(ctx context.Context, method string, params, result any) error {
	err := c.fakeCaller.Call(ctx, method, params, result)
	if method == "turn/start" {
		return errors.New("closed")
	}
	return err
}

func TestFailedChipFollowupReturnsToTheQueueWithItsPasteIntent(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		t.Run(paste.name, func(t *testing.T) {
			t.Parallel()
			caller := new(refusedStarts)
			m := runningChipDraftModel(t, caller, paste.text)
			m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			m = completeTurn(t, m)
			if m.thread.state != stateFailed {
				t.Fatalf("state = %q after the refused follow-up, want failed", m.thread.state)
			}
			m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
			if got := m.input.composer.Expanded(); got != paste.text {
				t.Fatalf("alt+up moved %q into the composer, want the pasted text", got)
			}
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			m = runCommand(t, m, command, "Enter did not send the returned follow-up")
			if m.confirm != nil {
				t.Fatalf("Enter on the returned follow-up opened %#v, want the prompt sent", m.confirm)
			}
			assertLastCallText(t, &caller.fakeCaller, 2, "turn/start", paste.text)
		})
	}
}

func TestSavedDraftRecordsTheIntentOfItsDraft(t *testing.T) {
	t.Parallel()
	chipFirst := editor{text: []rune{chipAtom(0)}, chips: []chip{{label: "[Paste #1 +11 lines]", text: "// a\nb"}}}
	recalled := newEditor()
	recalled.recall("// a\nb", true)
	edited := recalled
	edited.insert([]rune("x"))
	tests := []struct {
		name  string
		draft editor
		want  bool
	}{
		{name: "chip first", draft: chipFirst, want: true},
		{name: "typed double slash", draft: editor{text: []rune("//a b")}},
		{name: "typed command", draft: editor{text: []rune("/new")}},
		{name: "plain prompt", draft: editor{text: []rune("hello")}},
		{name: "recalled paste", draft: recalled, want: true},
		{name: "edited recalled paste", draft: edited},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			var h history
			h.saveDraft(test.draft, test.draft.Expanded())
			if len(h.entries) != 1 || h.entries[0].verbatim != test.want {
				t.Fatalf("entries = %#v, want one entry with verbatim %v", h.entries, test.want)
			}
		})
	}
}

func TestHistoryRecordsAMessageThatStartsWithASlashAsSentUnchanged(t *testing.T) {
	t.Parallel()
	tests := []struct {
		text string
		want bool
	}{
		{text: "// a\nb", want: true},
		{text: "/a b", want: true},
		{text: "/new", want: true},
		{text: "/", want: true},
		{text: "hello"},
		{text: "see /new now"},
	}
	for _, test := range tests {
		t.Run(test.text, func(t *testing.T) {
			t.Parallel()
			var h history
			h.add(test.text, "turn-1")
			if len(h.entries) != 1 || h.entries[0].verbatim != test.want {
				t.Fatalf("entries = %#v, want one entry with verbatim %v", h.entries, test.want)
			}
		})
	}
}

func assertComposer(t *testing.T, composer editor, value string, verbatim bool) {
	t.Helper()
	if composer.Value() != value || composer.verbatim() != verbatim {
		t.Fatalf("composer holds %q verbatim %v, want %q verbatim %v", composer.Value(), composer.verbatim(), value, verbatim)
	}
}

func TestHistoryRecallCarriesPasteIntentThroughDraftRestore(t *testing.T) {
	t.Parallel()
	const pasted = "// a\nb"
	chipFirst := editor{text: []rune{chipAtom(0)}, chips: []chip{{label: "[Paste #1 +2 lines]", text: pasted}}}
	var h history
	h.saveDraft(chipFirst, pasted)
	h.add("other", "")
	composer := newEditor()
	composer.SetValue("typed")
	h.older(&composer)
	assertComposer(t, composer, "other", false)
	h.older(&composer)
	assertComposer(t, composer, pasted, true)
	h.newer(&composer)
	h.newer(&composer)
	assertComposer(t, composer, "typed", false)
	composer.recall(pasted, true)
	h.older(&composer)
	h.newer(&composer)
	assertComposer(t, composer, pasted, true)
	composer.insert([]rune("x"))
	assertComposer(t, composer, pasted+"x", false)
}

func TestCtrlCSavesChipOnlyDraftExpanded(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(30)
	m := paletteModel(t)
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	if got := historyTexts(m); !slices.Equal(got, []string{pasted}) {
		t.Fatalf("history = %q, want the 30 pasted lines", got)
	}
}

func TestHistoryRecallRestoresChips(t *testing.T) {
	t.Parallel()
	first, second := pasteLines(12), pasteLines(13)
	m := paletteModel(t)
	m.input.history.add("earlier", "")
	m = typeText(t, m, "a ")
	m, _ = drive(t, m, tea.PasteMsg{Content: first})
	m = typeText(t, m, " b ")
	m, _ = drive(t, m, tea.PasteMsg{Content: second})
	original := m.input.composer.Expanded()
	m, _ = drive(t, m, press(tea.KeyHome, 0, ""))
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	if got := m.input.composer.Value(); got != "earlier" {
		t.Fatalf("up recalled %q, want earlier", got)
	}
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Expanded(); got != original {
		t.Fatalf("down restored %q, want %q", got, original)
	}
	if hasPrivateUse(m.input.composer.Expanded()) {
		t.Fatalf("restored draft holds a chip atom: %q", m.input.composer.Expanded())
	}
	if got := len(m.input.composer.chips); got != 2 {
		t.Fatalf("restored draft holds %d chips, want 2", got)
	}
	assertContains(t, dockText(m), "[Paste #1 +12 lines]", "[Paste #2 +13 lines]")
}

func TestHistoryRecallPastTwoEntriesRestoresTheDraftAndItsCaret(t *testing.T) {
	t.Parallel()
	first, second := pasteLines(12), pasteLines(13)
	m := paletteModel(t)
	m.input.history.add("older", "")
	m.input.history.add("newer", "")
	m = typeText(t, m, "a ")
	m, _ = drive(t, m, tea.PasteMsg{Content: first})
	m = typeText(t, m, " b ")
	m, _ = drive(t, m, tea.PasteMsg{Content: second})
	original := m.input.composer.Expanded()
	m, _ = drive(t, m, press(tea.KeyHome, 0, ""))
	for _, want := range []string{"newer", "older"} {
		m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
		if got := m.input.composer.Value(); got != want {
			t.Fatalf("up recalled %q, want %q", got, want)
		}
	}
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Value(); got != "newer" {
		t.Fatalf("down recalled %q, want newer", got)
	}
	m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
	if got := m.input.composer.Expanded(); got != original {
		t.Fatalf("down restored %q, want %q", got, original)
	}
	if got := len(m.input.composer.chips); got != 2 {
		t.Fatalf("restored draft holds %d chips, want 2", got)
	}
	if got, want := m.input.composer.cursor, len(m.input.composer.text); got != want {
		t.Fatalf("caret at %d after the restore, want the end of the draft at %d", got, want)
	}
	m = typeText(t, m, "z")
	if got := m.input.composer.Expanded(); got != original+"z" {
		t.Fatalf("typing after the restore gave %q, want the draft and z at its end", got)
	}
}

func TestHistorySearchRecallsCtrlCChipDraft(t *testing.T) {
	t.Parallel()
	pasted := pasteLines(30)
	want := "S7MARK " + pasted
	m := chipDraftModel(t, new(fakeCaller), pasted)
	m, _ = drive(t, m, press('c', tea.ModCtrl, ""))
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	if m.list == nil || m.list.kind != "history" {
		t.Fatal("ctrl+r did not open the history panel")
	}
	body := historyBody(m)
	if len(body) != 1 || !strings.HasPrefix(body[0], "S7MARK line 01 line 02") {
		t.Fatalf("history rows = %q, want the chip draft with its pasted lines", body)
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := m.input.composer.Expanded(); got != want {
		t.Fatalf("composer = %q after Enter, want %q", got, want)
	}
}

func TestHistoryRecallKeepsFrameInWindow(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	m.thread.id = "t1"
	var lines []string
	for i := range 20 {
		lines = append(lines, fmt.Sprintf("line %02d", i))
	}
	m.input.history.add(strings.Join(lines, "\n"), "")
	m, _ = drive(t, m, press(tea.KeyUp, 0, ""))
	frame := strings.Split(m.View().Content, "\n")
	assertFrame(t, frame, 40, 8)
	if got, want := m.input.composer.Value(), strings.Join(lines, "\n"); got != want {
		t.Fatalf("recalled draft = %q, want %q", got, want)
	}
	assertContains(t, m.View().Content, "line 19")
}

func TestHistoryHoldsSentPromptsWithTurn(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, "hello")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m, _ = drive(t, m, command())
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].turnID != "turn-1" {
		t.Fatalf("history = %#v, want one entry for turn-1", m.input.history.entries)
	}
}

func TestHistoryDropsConsecutiveRepeats(t *testing.T) {
	t.Parallel()
	var h history
	h.add("same", "")
	h.add("same", "")
	h.add("other", "")
	h.add("same", "")
	if len(h.entries) != 3 {
		t.Fatalf("entries = %#v, want three", h.entries)
	}
}

func TestHistoryCapsAtTwoHundred(t *testing.T) {
	t.Parallel()
	var h history
	for i := range 205 {
		h.add(fmt.Sprintf("prompt %d", i), "")
	}
	if len(h.entries) != 200 || h.entries[0].text != "prompt 5" {
		t.Fatalf("entries = %d, first %q; want 200 starting at prompt 5", len(h.entries), h.entries[0].text)
	}
}

func TestHistoryFromUserMessageItem(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.thread.id = "t1"
	m, _ = drive(t, m, notificationMsg{method: "item/completed", params: []byte(`{"threadId":"t1","item":{"id":"u1","type":"userMessage","text":"from the server"}}`)})
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].text != "from the server" {
		t.Fatalf("history = %#v, want the server message", m.input.history.entries)
	}
}

func TestHistoryFromResumedThread(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m.applyResumed(&resumedThread{Thread: rpc.Thread{ID: "t1", Turns: []rpc.Turn{{ID: "turn-1", Items: []rpc.Item{{ID: "u1", Type: "userMessage", Text: "earlier"}}}}}})
	if n := len(m.input.history.entries); n != 1 || m.input.history.entries[0].turnID != "turn-1" {
		t.Fatalf("history = %#v, want the resumed prompt with turn-1", m.input.history.entries)
	}
}

var threadSources = []struct {
	name string
	load func(*testing.T, model, string) model
}{
	{name: "resume", load: func(t *testing.T, m model, text string) model {
		t.Helper()
		thread := rpc.Thread{ID: "t1", Turns: []rpc.Turn{{ID: "turn-1", Status: "completed", Items: []rpc.Item{{ID: "u1", Type: "userMessage", Text: text}}}}}
		m, _ = drive(t, m, resultMsg{op: "resumed", value: &resumedThread{Thread: thread}})
		return m
	}},
	{name: "echo", load: func(t *testing.T, m model, text string) model {
		t.Helper()
		m.thread.id = "t1"
		echo := fmt.Appendf(nil, `{"threadId":"t1","item":{"id":"u1","type":"userMessage","text":%q}}`, text)
		m, _ = drive(t, m, notificationMsg{method: "item/completed", params: echo})
		return m
	}},
}

type threadPromptCase struct {
	name string
	text string
	keys []tea.KeyPressMsg
	sent string
}

var threadPromptCases = []threadPromptCase{
	{name: "double slash", text: "// Render the menu\nline", sent: "// Render the menu\nline"},
	{name: "command with a word", text: "/new now\nline", sent: "/new now\nline"},
	{name: "bare command", text: "/new\nline", sent: "/new\nline"},
	{name: "bare slash", text: "/", sent: "/"},
	{name: "plain prompt", text: "plain\nline", sent: "plain\nline"},
	{name: "slash inside", text: "see /new now\nline", sent: "see /new now\nline"},
	{name: "edited command", text: "/new now\nline", keys: []tea.KeyPressMsg{press('x', 0, "x")}},
}

type threadPromptRun struct {
	name   string
	prompt threadPromptCase
	load   func(*testing.T, model, string) model
	recall func(*testing.T, model) model
}

func threadPromptRuns() []threadPromptRun {
	var runs []threadPromptRun
	for _, source := range threadSources {
		for _, recall := range historyRecalls {
			for _, prompt := range threadPromptCases {
				runs = append(runs, threadPromptRun{name: source.name + "/" + recall.name + "/" + prompt.name, prompt: prompt, load: source.load, recall: recall.run})
			}
		}
	}
	return runs
}

func (r threadPromptRun) enter(t *testing.T, caller *fakeCaller) (model, tea.Cmd) {
	t.Helper()
	m := newTestModel(caller, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m = r.load(t, m, r.prompt.text)
	if n := len(m.input.history.entries); n != 1 {
		t.Fatalf("history holds %d entries after the load, want 1", n)
	}
	m = r.recall(t, m)
	for _, key := range r.prompt.keys {
		m, _ = drive(t, m, key)
	}
	return drive(t, m, press(tea.KeyEnter, 0, ""))
}

func (r threadPromptRun) check(t *testing.T) {
	t.Helper()
	caller := new(fakeCaller)
	m, command := r.enter(t, caller)
	if r.prompt.sent == "" {
		if len(caller.calls) != 0 || m.confirm == nil || m.confirm.kind != confirmNew {
			t.Fatalf("calls = %#v, confirm = %#v, want the new-thread confirmation of the typed /new and no request", caller.calls, m.confirm)
		}
		return
	}
	m = runCommand(t, m, command, "Enter did not send the recalled prompt; notice: "+m.notice)
	if m.confirm != nil {
		t.Fatalf("Enter on the recalled prompt opened %#v, want the prompt sent", m.confirm)
	}
	assertLastCallText(t, caller, 1, "turn/start", r.prompt.sent)
}

func TestThreadPromptStartingWithSlashIsRecalledSentUnchanged(t *testing.T) {
	t.Parallel()
	for _, run := range threadPromptRuns() {
		t.Run(run.name, func(t *testing.T) {
			t.Parallel()
			run.check(t)
		})
	}
}

func historyPanel(t *testing.T, prompts ...string) model {
	t.Helper()
	m := panelPicker(t, new(fakeCaller))
	for _, prompt := range prompts {
		m.input.history.add(prompt, "")
	}
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	if m.list == nil || m.list.kind != "history" {
		t.Fatal("ctrl+r did not open the history panel")
	}
	return m
}

func historyBody(m model) []string {
	return plainBody(m.listPanelView().body)
}

func historyTexts(m model) []string {
	texts := make([]string, 0, len(m.input.history.entries))
	for _, entry := range m.input.history.entries {
		texts = append(texts, entry.text)
	}
	return texts
}

func numberedPrompts(count int) []string {
	prompts := make([]string, 0, count)
	for i := range count {
		prompts = append(prompts, fmt.Sprintf("prompt %03d", i))
	}
	return prompts
}

func userMessageNotification(text string) notificationMsg {
	return notificationMsg{method: "item/completed", params: fmt.Appendf(nil, `{"threadId":"t1","item":{"id":"u-%s","type":"userMessage","text":%q}}`, text, text)}
}

func TestPromptHistorySearch(t *testing.T) {
	t.Parallel()
	m := historyPanel(t, "add the parser", "fix the decoder", "write docs", "refactor loader", "fix flaky test")
	if m.list.title != "history" {
		t.Fatalf("panel title = %q, want history", m.list.title)
	}
	newest := []string{"fix flaky test", "refactor loader", "write docs", "fix the decoder", "add the parser"}
	if got := historyBody(m); !slices.Equal(got, newest) {
		t.Fatalf("history rows = %q, want the five prompts newest first %q", got, newest)
	}
	if got := len(m.panelLines()); got != 12 {
		t.Fatalf("history panel has %d rows, want 12", got)
	}
	assertContains(t, dockText(m), "┌ history", "type to filter · Enter load · Delete remove · Esc close", "filter:")
	m = typeText(t, m, "fix")
	if got, want := historyBody(m), []string{"fix flaky test", "fix the decoder"}; !slices.Equal(got, want) {
		t.Fatalf("rows after filter fix = %q, want %q", got, want)
	}
	assertContains(t, dockText(m), "┃ ● fix flaky test")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	assertContains(t, dockText(m), "┃ ● fix the decoder")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := m.input.composer.Value(); got != "fix the decoder" {
		t.Fatalf("composer = %q after Enter, want the highlighted prompt", got)
	}
	if m.list != nil {
		t.Fatal("history panel stayed open after Enter")
	}
	if got := len(m.input.history.entries); got != 5 {
		t.Fatalf("Enter changed the history to %d entries, want 5", got)
	}
}

func TestPromptHistoryDelete(t *testing.T) {
	t.Parallel()
	m := historyPanel(t, "add the parser", "fix the decoder", "write docs", "fix flaky test")
	m.input.composer.SetValue("keep me")
	m = typeText(t, m, "fix")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDelete})
	if got, want := historyTexts(m), []string{"add the parser", "write docs", "fix flaky test"}; !slices.Equal(got, want) {
		t.Fatalf("history = %q, want only the highlighted fix the decoder removed %q", got, want)
	}
	if m.list == nil || !slices.Equal(historyBody(m), []string{"fix flaky test"}) {
		t.Fatalf("panel rows = %q, want the one remaining match", historyBody(m))
	}
	if got := m.input.composer.Value(); got != "keep me" {
		t.Fatalf("composer = %q after Delete, want the draft kept", got)
	}
	if m.input.history.index != len(m.input.history.entries) {
		t.Fatalf("history index = %d, want the end %d", m.input.history.index, len(m.input.history.entries))
	}
}

func TestPromptHistoryBackspaceRemovesOnlyOnEmptyFilter(t *testing.T) {
	t.Parallel()
	m := historyPanel(t, "alpha", "beta")
	m = typeText(t, m, "be")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	if got := m.input.field.value(); got != "b" || len(m.input.history.entries) != 2 {
		t.Fatalf("backspace with a filter left filter %q and %d entries, want b and 2", got, len(m.input.history.entries))
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	if got, want := historyTexts(m), []string{"alpha"}; !slices.Equal(got, want) {
		t.Fatalf("history = %q, want beta removed by backspace on the empty filter %q", got, want)
	}
}

func TestPromptHistoryEscRestoresDraft(t *testing.T) {
	t.Parallel()
	m := panelPicker(t, new(fakeCaller))
	m.input.history.add("older prompt", "")
	m.input.composer.SetValue("half typed\nsecond line")
	m.input.composer.cursor = 4
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	m = typeText(t, m, "zz")
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.list == nil || m.input.field.value() != "" {
		t.Fatalf("first esc left list %v and filter %q, want the list open and the filter cleared", m.list, m.input.field.value())
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.list != nil {
		t.Fatal("second esc left the history panel open")
	}
	if got := m.input.composer.Value(); got != "half typed\nsecond line" || m.input.composer.cursor != 4 {
		t.Fatalf("composer = %q cursor %d, want the saved draft with cursor 4", got, m.input.composer.cursor)
	}
}

func TestPromptHistoryLoadsFullPrompt(t *testing.T) {
	t.Parallel()
	full := "first line\n\tsecond line " + strings.Repeat("x", 3000)
	m := historyPanel(t, full)
	m.input.composer.SetValue("draft")
	item, ok := m.highlighted()
	if !ok || len(item.label) > 1024 || strings.ContainsAny(item.label, "\n\t") || !strings.HasPrefix(item.label, "first line second line x") {
		t.Fatalf("label = %q (%d bytes), want one bounded row that starts with the prompt", item.label, len(item.label))
	}
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	if got := m.input.composer.Value(); got != full {
		t.Fatalf("composer holds %d bytes, want the full %d byte prompt", len(got), len(full))
	}
}

func TestPromptHistoryEmptyHasNoSelectableRow(t *testing.T) {
	t.Parallel()
	m := panelPicker(t, new(fakeCaller))
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	if got := historyBody(m); !slices.Equal(got, []string{"No prompt history yet."}) {
		t.Fatalf("rows = %q, want only the empty message", got)
	}
	assertContains(t, dockText(m), "┌ history", "No prompt history yet.")
	if _, ok := m.highlighted(); ok {
		t.Fatal("the empty message is selectable")
	}
	if got := len(m.panelLines()); got != 12 {
		t.Fatalf("empty history panel has %d rows, want 12", got)
	}
}

func TestPromptHistoryEmptyKeysDoNothing(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := panelPicker(t, caller)
	m.input.composer.SetValue("draft")
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	for _, key := range []tea.KeyPressMsg{{Code: tea.KeyEnter}, {Code: tea.KeyDelete}, {Code: tea.KeyBackspace}} {
		m, _ = drive(t, m, key)
	}
	if m.list == nil || m.input.composer.Value() != "draft" || len(caller.calls) != 0 {
		t.Fatalf("an empty panel changed state: list %v composer %q calls %d", m.list, m.input.composer.Value(), len(caller.calls))
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	if m.list != nil || m.input.composer.Value() != "draft" {
		t.Fatalf("esc left list %v and composer %q, want the panel closed and the draft kept", m.list, m.input.composer.Value())
	}
}

func TestPromptHistoryDeleteSurvivesAsyncAdd(t *testing.T) {
	t.Parallel()
	m := historyPanel(t, "p1", "p2", "p3")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	if item, ok := m.highlighted(); !ok || item.label != "p1" {
		t.Fatalf("highlighted = %q, want p1", item.label)
	}
	m, _ = drive(t, m, userMessageNotification("p4"))
	if got, want := historyBody(m), []string{"p4", "p3", "p2", "p1"}; !slices.Equal(got, want) {
		t.Fatalf("rows after an async add = %q, want %q", got, want)
	}
	if item, ok := m.highlighted(); !ok || item.label != "p1" {
		t.Fatalf("highlighted = %q after the add, want p1 kept", item.label)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDelete})
	if got, want := historyTexts(m), []string{"p2", "p3", "p4"}; !slices.Equal(got, want) {
		t.Fatalf("history = %q, want only p1 removed %q", got, want)
	}
}

func TestPromptHistoryDeleteSurvivesCapEviction(t *testing.T) {
	t.Parallel()
	prompts := numberedPrompts(200)
	m := historyPanel(t, prompts...)
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyUp})
	if item, ok := m.highlighted(); !ok || item.label != "prompt 000" {
		t.Fatalf("highlighted = %q, want the oldest prompt 000", item.label)
	}
	m, _ = drive(t, m, userMessageNotification("prompt 200"))
	if len(m.input.history.entries) != 200 || slices.Contains(historyTexts(m), "prompt 000") {
		t.Fatalf("history holds %d entries and prompt 000 = %v, want 200 and evicted", len(m.input.history.entries), slices.Contains(historyTexts(m), "prompt 000"))
	}
	shown, ok := m.highlighted()
	if !ok {
		t.Fatal("no row is highlighted after the eviction")
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDelete})
	want := slices.DeleteFunc(append(slices.Clone(prompts[1:]), "prompt 200"), func(text string) bool { return text == shown.label })
	if got := historyTexts(m); !slices.Equal(got, want) {
		t.Fatalf("history after Delete holds %d entries, want only the shown %q removed (%d entries)", len(got), shown.label, len(want))
	}
}

func staleHistoryPanel(t *testing.T) (model, []string) {
	t.Helper()
	prompts := numberedPrompts(200)
	m := historyPanel(t, prompts...)
	m.list.cursor = len(m.list.items) - 1
	m.input.history.add("prompt 200", "")
	return m, append(slices.Clone(prompts[1:]), "prompt 200")
}

func TestPromptHistoryStaleLoadTakesNoAction(t *testing.T) {
	t.Parallel()
	m, want := staleHistoryPanel(t)
	m.input.composer.SetValue("keep")
	stale, ok := m.highlighted()
	if !ok || stale.label != "prompt 000" {
		t.Fatalf("stale row = %q, want the evicted prompt 000", stale.label)
	}
	if command := m.loadHistory(stale); command != nil || m.input.composer.Value() != "keep" || m.list == nil {
		t.Fatalf("loading an evicted entry gave composer %q list %v, want no action", m.input.composer.Value(), m.list)
	}
	if got := historyTexts(m); !slices.Equal(got, want) {
		t.Fatalf("loading an evicted entry changed the history to %d entries", len(got))
	}
	if got := m.list.items[0].label; got != "prompt 200" {
		t.Fatalf("panel top row = %q, want the list rebuilt with prompt 200 first", got)
	}
}

func TestPromptHistoryStaleDeleteTakesNoAction(t *testing.T) {
	t.Parallel()
	m, want := staleHistoryPanel(t)
	m.removeHistory()
	if got := historyTexts(m); !slices.Equal(got, want) {
		t.Fatalf("removing an evicted entry changed the history to %d entries, want all 200 kept", len(got))
	}
	if got := m.list.items[0].label; got != "prompt 200" {
		t.Fatalf("panel top row = %q, want the list rebuilt with prompt 200 first", got)
	}
	m.removeHistory()
	if got, kept := historyTexts(m), want[1:]; !slices.Equal(got, kept) {
		t.Fatalf("removing the rebuilt highlight left %d entries, want exactly the oldest prompt 001 removed", len(got))
	}
}

func TestPromptHistoryClampsInSmallTerminal(t *testing.T) {
	t.Parallel()
	m := panelPicker(t, new(fakeCaller))
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 40, Height: 8})
	for _, prompt := range numberedPrompts(30) {
		m.input.history.add(prompt, "")
	}
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	assertFrame(t, strings.Split(m.View().Content, "\n"), 40, 8)
	keys := make([]tea.KeyPressMsg, 40)
	for i := range keys {
		keys[i] = tea.KeyPressMsg{Code: tea.KeyDown}
	}
	m = assertSteadyDock(t, m, keys)
	assertFrame(t, strings.Split(m.View().Content, "\n"), 40, 8)
}

func TestCtrlROpensHistoryOnlyInPromptMode(t *testing.T) {
	t.Parallel()
	m := panelPicker(t, new(fakeCaller))
	m.input.history.add("earlier", "")
	m, _ = drive(t, m, press('!', 0, "!"))
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	if m.list != nil {
		t.Fatal("ctrl+r opened the history panel in shell mode")
	}
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
	if m.list == nil || m.list.kind != "history" {
		t.Fatal("ctrl+r did not open the history panel in prompt mode")
	}
}

func TestHelpListsHistorySearchUnderCompose(t *testing.T) {
	t.Parallel()
	m := helpModel(t)
	body := plainBody(m.helpBody())
	compose, turn := slices.Index(body, "Compose"), slices.Index(body, "Turn")
	at := slices.IndexFunc(body, func(line string) bool { return strings.HasPrefix(line, "  ctrl+r") })
	if compose < 0 || turn < 0 || at < compose || at > turn {
		t.Fatalf("ctrl+r row at %d, want it between Compose %d and Turn %d", at, compose, turn)
	}
	if !strings.Contains(body[at], "opens the prompt history search") {
		t.Fatalf("ctrl+r row = %q, want the history search action", body[at])
	}
	for _, line := range body {
		if strings.Contains(line, "advisor reset") {
			t.Fatalf("help still mentions the advisor reset key: %q", line)
		}
	}
}

func TestHistoryEntryIdentityIgnoresRepeats(t *testing.T) {
	t.Parallel()
	var h history
	h.add("a", "")
	h.add("a", "turn-1")
	h.add("b", "")
	if len(h.entries) != 2 || h.entries[0].id != 1 || h.entries[1].id != 2 {
		t.Fatalf("entries = %#v, want ids 1 and 2 with the repeat of a taking no new id", h.entries)
	}
}

func TestHistoryEntryIdentitySurvivesCap(t *testing.T) {
	t.Parallel()
	var h history
	for i := range 207 {
		h.add(fmt.Sprintf("prompt %d", i), "")
	}
	if len(h.entries) != 200 || h.entries[0].id != 8 || h.entries[199].id != 207 {
		t.Fatalf("ids run %d to %d over %d entries, want 8 to 207 over 200", h.entries[0].id, h.entries[len(h.entries)-1].id, len(h.entries))
	}
}

func TestHistoryAllEntriesIsACopy(t *testing.T) {
	t.Parallel()
	var h history
	h.add("a", "")
	got := h.allEntries()
	got[0].text = "changed"
	if h.entries[0].text != "a" {
		t.Fatalf("history entry = %q after the snapshot changed, want a", h.entries[0].text)
	}
}

func TestHistoryRemoveBounds(t *testing.T) {
	t.Parallel()
	var h history
	h.add("a", "")
	h.add("b", "")
	if h.remove(-1) || h.remove(2) || len(h.entries) != 2 {
		t.Fatalf("remove outside the entries changed %#v", h.entries)
	}
	h.index = 0
	if !h.remove(0) || len(h.entries) != 1 || h.entries[0].text != "b" || h.index != 1 {
		t.Fatalf("remove(0) left %#v index %d, want b alone and the index at the end", h.entries, h.index)
	}
}

func TestHistoryRemoveClearsAwaitingTurn(t *testing.T) {
	t.Parallel()
	var kept history
	kept.add("a", "")
	kept.add("b", "")
	kept.startTurn("turn-9")
	if kept.entries[1].turnID != "turn-9" || kept.entries[0].turnID != "" {
		t.Fatalf("entries = %#v, want turn-9 on b only", kept.entries)
	}
	var removed history
	removed.add("a", "")
	removed.add("b", "")
	removed.remove(1)
	removed.startTurn("turn-9")
	if removed.entries[0].turnID != "" {
		t.Fatalf("entries = %#v, want the removed entry's turn kept away from a", removed.entries)
	}
}

func TestDeleteStillEditsFilterOutsideHistory(t *testing.T) {
	t.Parallel()
	m := panelPicker(t, new(fakeCaller))
	m.input.history.add("keep me", "")
	m.openList(&listPanel{kind: "tree", title: "tree", items: []listItem{{id: "1", label: "alpha"}}})
	m = typeText(t, m, "ab")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyLeft})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDelete})
	if got := m.input.field.value(); got != "a" {
		t.Fatalf("filter = %q after Delete, want a", got)
	}
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	if got := m.input.field.value(); got != "" || m.list == nil || len(m.input.history.entries) != 1 {
		t.Fatalf("filter %q list %v history %d, want an empty filter, the list open and the history kept", got, m.list, len(m.input.history.entries))
	}
}
