package tui

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
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
	return m.listPanelView().body
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
	assertContains(t, dockText(m), "┃ fix flaky test")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	assertContains(t, dockText(m), "┃ fix the decoder")
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
	body := m.helpBody()
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
