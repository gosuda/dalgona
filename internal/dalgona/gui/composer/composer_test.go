//go:build gui

package composer

import (
	"bytes"
	"context"
	"image"
	"image/png"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func idleHarness(t *testing.T, host Host) *harness {
	t.Helper()
	h := newHarness(t, host)
	h.view.openThread("")
	h.focus()
	return h
}

func tenLines() string { return strings.Repeat("row\r\n", pasteChipLines) }

func TestEnterSendsTypedTextThenPastedChip(t *testing.T) {
	h := idleHarness(t, Host{}).withClipboard(platform.ClipboardContent{Text: tenLines()})
	h.typeText("hello")
	h.press("V", key.ModShortcut)
	h.awaitWake()
	if h.text() != "hello" || !h.shows("Attachments") {
		t.Fatalf("a ten-line paste must become a chip; field %q", h.text())
	}
	h.press(key.NameReturn, 0)
	sends := intentsOf[state.Send](h)
	if len(sends) != 1 {
		t.Fatalf("sends = %+v", sends)
	}
	requirePastedSend(h, sends[0])
	if h.text() != "" || h.shows("Attachments") {
		t.Fatalf("a sent message must leave the field: %q", h.text())
	}
}

func requirePastedSend(h *harness, got state.Send) {
	h.t.Helper()
	if got.Key != testKey || got.Draft.Text != "hello" || len(got.Draft.Attachments) != 1 || got.Draft.Attachments[0].Text != strings.Repeat("row\n", pasteChipLines) {
		h.t.Fatalf("send = %+v", got)
	}
	if put := lastPut(h); put.Revision != got.Draft.Revision || put.Text != "hello" {
		h.t.Fatalf("the sent generation must be stored first: put %+v send revision %d", put, got.Draft.Revision)
	}
}

func TestShortPasteStaysInTheField(t *testing.T) {
	h := idleHarness(t, Host{}).withClipboard(platform.ClipboardContent{Text: "a\r\nb"})
	h.press("V", key.ModShortcut)
	h.awaitWake()
	if h.text() != "a\nb" || h.shows("Attachments") {
		t.Fatalf("a two-line paste must be inserted as text, got %q", h.text())
	}
}

func TestPasteUsesAReaderThatAppearsAfterStartup(t *testing.T) {
	var reader platform.ClipboardReader
	h := idleHarness(t, Host{})
	h.env.Clipboard = func() platform.ClipboardReader { return reader }
	reader = clipboardStub{content: platform.ClipboardContent{Text: "from the owned reader"}}
	h.press("V", key.ModShortcut)
	h.awaitWake()
	if h.text() != "from the owned reader" {
		t.Fatalf("a reader attached after startup was not used; field %q", h.text())
	}
}

func TestEnterSteersRunningTurnAndKeepsChips(t *testing.T) {
	h := newHarness(t, Host{}).withClipboard(platform.ClipboardContent{Text: tenLines()})
	h.view.openThread("turn-1")
	h.focus()
	h.press("V", key.ModShortcut)
	h.awaitWake()
	h.typeText("look at the log")
	h.press(key.NameReturn, 0)
	steers := intentsOf[state.Steer](h)
	if len(steers) != 1 || steers[0].Draft.Text != "look at the log" || len(steers[0].Draft.Attachments) != 0 || len(intentsOf[state.Send](h)) != 0 {
		t.Fatalf("steer = %+v", steers)
	}
	if !h.shows("Attachments") {
		t.Fatal("steering must keep the chips for the next complete send")
	}
}

func TestAltEnterQueuesWhileRunning(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("turn-1")
	h.focus()
	h.typeText("next step")
	h.press(key.NameReturn, key.ModAlt)
	queued := intentsOf[state.Enqueue](h)
	if len(queued) != 1 || queued[0].Draft.Text != "next step" || len(intentsOf[state.Steer](h)) != 0 {
		t.Fatalf("enqueue = %+v intents %+v", queued, h.intents)
	}
}

func TestQueueFullPreservesField(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("turn-1")
	for i := range followUpLimit {
		h.view.followUps[testKey] = append(h.view.followUps[testKey], state.FollowUp{ID: uint64(i + 1), Text: "x"})
	}
	h.focus()
	h.typeText("one more")
	h.press(key.NameReturn, key.ModAlt)
	if len(intentsOf[state.Enqueue](h)) != 0 || h.text() != "one more" || !h.shows(noticeQueueFull) {
		t.Fatalf("a full queue must refuse and keep the field: %q", h.text())
	}
}

func TestEscapeArmsGuardThenInterrupts(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("turn-1")
	h.focus()
	h.press(key.NameEscape, 0)
	if len(intentsOf[state.Interrupt](h)) != 0 || !h.showsPrefix(helpArmed) {
		t.Fatal("the first Escape must only arm the guard and say so")
	}
	h.wait(interruptWindow + time.Millisecond)
	h.press(key.NameEscape, 0)
	if len(intentsOf[state.Interrupt](h)) != 0 {
		t.Fatal("an Escape after the guard expired must re-arm, not interrupt")
	}
	h.press(key.NameEscape, 0)
	got := intentsOf[state.Interrupt](h)
	if len(got) != 1 || got[0].Thread != testThreadKey || got[0].Window != testWindow {
		t.Fatalf("interrupt = %+v", got)
	}
}

func TestEscapeDuringRateLimitWaitInterruptsAtOnce(t *testing.T) {
	h := newHarness(t, Host{})
	tv := h.view.openThread("turn-1")
	tv.Turns = []state.TurnView{{ID: "turn-1", RetryError: &rpc.TurnError{Message: "rate limited"}}}
	h.focus()
	h.press(key.NameEscape, 0)
	if len(intentsOf[state.Interrupt](h)) != 1 {
		t.Fatal("Escape in a rate-limit wait must interrupt immediately")
	}
}

func TestEscapeInIdleThreadKeepsDraft(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("keep me")
	before := len(h.intents)
	h.press(key.NameEscape, 0)
	if h.text() != "keep me" || len(h.intents) != before {
		t.Fatalf("Escape in an idle thread changed the draft: %q %+v", h.text(), h.intents[before:])
	}
}

func TestRecallBrowsesAndRestoresTheDraft(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("")
	h.view.prompts[testThreadKey] = []string{"first prompt", "second prompt"}
	h.focus()
	h.press(key.NameUpArrow, 0)
	if h.text() != "second prompt" || h.dock.editor.Draft().Selection.End != 0 {
		t.Fatalf("recall = %q at %+v", h.text(), h.dock.editor.Draft().Selection)
	}
	h.press(key.NameUpArrow, 0)
	if h.text() != "first prompt" {
		t.Fatalf("older recall = %q", h.text())
	}
	h.press(key.NameDownArrow, 0)
	h.press(key.NameDownArrow, 0)
	if h.text() != "" {
		t.Fatalf("browsing past the newest must restore the empty draft, got %q", h.text())
	}
	if len(intentsOf[state.PutDraft](h)) != 0 {
		t.Fatal("browsing history must not replace the stored draft")
	}
}

func TestEditingRecalledTextMakesItTheDraft(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("")
	h.view.prompts[testThreadKey] = []string{"run tests"}
	h.focus()
	h.press(key.NameUpArrow, 0)
	h.router.Queue(key.EditEvent{Range: key.Range{Start: 0, End: 0}, Text: "please "})
	h.frame()
	h.frame()
	if put := lastPut(h); put.Text != "please run tests" {
		t.Fatalf("stored draft = %q", put.Text)
	}
}

func TestSlashCommandOpensPanelAndClearsField(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("/mo")
	if !h.shows("Commands") || !h.shows("/model") {
		t.Fatal("a leading slash must list matching commands")
	}
	h.press(key.NameReturn, 0)
	if len(h.routes) != 1 || h.routes[0] != (shell.Route{Panel: "model", Thread: testThreadKey}) {
		t.Fatalf("routes = %+v", h.routes)
	}
	if h.text() != "" || len(intentsOf[state.Send](h)) != 0 {
		t.Fatalf("opening a command must clear the field and send nothing: %q", h.text())
	}
}

func TestRecallNeedsCaretAtStartWhenTheKeyArrives(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("")
	h.view.prompts[testThreadKey] = []string{"older"}
	h.focus()
	h.typeText("abc")
	h.router.Queue(key.SelectionEvent{Start: 0, End: 0})
	h.frame()
	h.frame()
	h.router.Queue(key.SelectionEvent{Start: 3, End: 3}, key.Event{Name: key.NameUpArrow, State: key.Press})
	h.frame()
	h.frame()
	if h.text() != "abc" {
		t.Fatalf("ArrowUp away from the start recalled %q", h.text())
	}
}

func TestPaletteArrowsChooseTheHighlightedCommand(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("/re")
	h.press(key.NameDownArrow, 0)
	h.press(key.NameTab, 0)
	if len(h.routes) != 1 || h.routes[0].Panel != "revert" {
		t.Fatalf("routes = %+v", h.routes)
	}
}

func TestPaletteEscapeKeepsFieldAndPartialNameOpensNothing(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("/mo")
	h.press(key.NameEscape, 0)
	if h.shows("Commands") || h.text() != "/mo" {
		t.Fatalf("Escape must close the list and keep %q", h.text())
	}
	h.press(key.NameReturn, 0)
	if len(h.routes) != 0 || len(intentsOf[state.Send](h)) != 0 || !h.shows(unknownCommand("mo")) {
		t.Fatal("a partial command name must explain the escape and neither open nor send")
	}
}

func TestUnknownCommandExplainsEscape(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("/zz")
	if !h.showsPrefix("No command named /zz. Start with //") {
		t.Fatal("an unknown command must name itself and the // escape")
	}
	h.press(key.NameReturn, 0)
	if len(intentsOf[state.Send](h)) != 0 || len(h.routes) != 0 {
		t.Fatal("an unknown command must not send or open")
	}
}

func TestSlashEscapeAndPathsSendAsMessages(t *testing.T) {
	for _, c := range []struct{ typed, sent string }{
		{typed: "//model is great", sent: "/model is great"},
		{typed: "/etc/hosts is wrong", sent: "/etc/hosts is wrong"},
	} {
		h := idleHarness(t, Host{})
		h.typeText(c.typed)
		h.press(key.NameReturn, 0)
		sends := intentsOf[state.Send](h)
		if len(sends) != 1 || sends[0].Draft.Text != c.sent || len(h.routes) != 0 {
			t.Fatalf("%q sent %+v routes %+v", c.typed, sends, h.routes)
		}
	}
}

func TestCommandWithTextAfterSpaceOpensNothing(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("/model gpt")
	if h.shows("Commands") {
		t.Fatal("text after a space must hide command matches")
	}
	h.press(key.NameReturn, 0)
	if len(h.routes) != 0 || len(intentsOf[state.Send](h)) != 0 || h.text() != "/model gpt" {
		t.Fatalf("routes %+v field %q", h.routes, h.text())
	}
}

func TestDisabledCommandStatesItsPrerequisite(t *testing.T) {
	h := idleHarness(t, Host{})
	h.view.threads[testThreadKey].ActiveTurn = "turn-1"
	h.typeText("/compact")
	h.press(key.NameReturn, 0)
	if len(h.routes) != 0 || !h.shows(reasonNeedsIdle) || h.text() != "/compact" {
		t.Fatalf("a refused command must state its prerequisite and keep the field: routes %+v", h.routes)
	}
}

func searchReady(h *harness, paths ...string) state.SearchFiles {
	h.t.Helper()
	h.wait(settleDelay)
	searches := intentsOf[state.SearchFiles](h)
	if len(searches) == 0 {
		h.t.Fatal("no file search was requested")
	}
	req := searches[len(searches)-1]
	h.view.searches[testKey] = state.FileSearch{Ticket: req.Ticket, Query: req.Query, Root: "/home/dev/project", Phase: state.SearchDone, Paths: paths}
	h.frame()
	h.frame()
	return req
}

func TestFileCompletionWaitsThenInsertsPath(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("see @ma")
	if len(intentsOf[state.SearchFiles](h)) != 0 {
		t.Fatal("the search must wait 120 ms after the latest keystroke")
	}
	req := searchReady(h, "src/main.go", "docs/manual.md")
	if req.Key != testKey || req.Query != "ma" {
		t.Fatalf("search = %+v", req)
	}
	if !h.shows("Files") || !h.shows("main.go") {
		t.Fatal("results must list file names")
	}
	h.press(key.NameReturn, 0)
	if h.text() != "see @src/main.go " || len(intentsOf[state.Send](h)) != 0 {
		t.Fatalf("field %q sends %d", h.text(), len(intentsOf[state.Send](h)))
	}
}

func TestFileCompletionPendingQueryNeverSends(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("@cfg")
	h.wait(settleDelay)
	h.press(key.NameReturn, 0)
	h.press(key.NameTab, 0)
	if len(intentsOf[state.Send](h)) != 0 || h.text() != "@cfg" {
		t.Fatalf("Enter during a pending search sent or changed %q", h.text())
	}
}

func TestFileCompletionDiscardsStaleResults(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("@ma")
	old := searchReady(h, "src/main.go")
	h.typeText("i")
	if h.shows("main.go") {
		t.Fatal("a query edit must clear old results immediately")
	}
	h.view.searches[testKey] = state.FileSearch{Ticket: old.Ticket, Query: "mai", Phase: state.SearchDone, Paths: []string{"stale/main.go"}}
	h.wait(settleDelay)
	if h.shows("main.go") {
		t.Fatal("a result for an older ticket must be discarded")
	}
	searches := intentsOf[state.SearchFiles](h)
	if len(searches) != 2 || searches[1].Query != "mai" || searches[1].Ticket == old.Ticket {
		t.Fatalf("searches = %+v", searches)
	}
}

func TestFileCompletionIgnoresEmptyQueryAndCaretInSameWord(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("@")
	h.wait(settleDelay)
	if len(intentsOf[state.SearchFiles](h)) != 0 {
		t.Fatal("an empty query must send no request")
	}
	h.typeText("abc")
	searchReady(h, "abc.txt")
	h.press(key.NameLeftArrow, 0)
	h.wait(settleDelay)
	if n := len(intentsOf[state.SearchFiles](h)); n != 1 {
		t.Fatalf("caret movement inside the word searched again: %d requests", n)
	}
}

func TestFileCompletionShowsPermissionState(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("@ma")
	h.wait(settleDelay)
	req := intentsOf[state.SearchFiles](h)[0]
	h.view.searches[testKey] = state.FileSearch{Ticket: req.Ticket, Query: req.Query, Phase: state.SearchAuthorizing}
	h.frame()
	if !h.shows("Waiting for read access to the workspace") {
		t.Fatal("pending authority must stay visible")
	}
	h.view.searches[testKey] = state.FileSearch{Ticket: req.Ticket, Query: req.Query, Phase: state.SearchDone, Incomplete: true, Paths: []string{"a/main.go"}}
	h.frame()
	if !h.shows("Partial results; the search stopped before checking every file") {
		t.Fatal("an incomplete search must say its results are partial")
	}
}

func TestShellCommandRunsThroughWorkspaceExec(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("!  ls -la")
	if !h.shows(labelShell) || !h.showsPrefix("Enter runs the command in /home/dev/project. It is not sent to the agent.") {
		t.Fatal("a leading ! must relabel the field and explain where it runs")
	}
	h.press(key.NameReturn, 0)
	runs := intentsOf[state.RunShell](h)
	if len(runs) != 1 || runs[0].Command != "  ls -la" || runs[0].Key != testKey || len(intentsOf[state.Send](h)) != 0 {
		t.Fatalf("runs = %+v", runs)
	}
	h.view.runs[testKey] = []state.ShellRun{{Ticket: runs[0].Ticket, N: 1, Command: "  ls -la", Phase: state.ShellAuthorizing}}
	h.frame()
	if h.text() != "!  ls -la" {
		t.Fatal("the draft must stay while permission is pending")
	}
	h.view.runs[testKey][0].Phase = state.ShellRunning
	h.frame()
	if h.text() != "" {
		t.Fatalf("a started command must leave the field, got %q", h.text())
	}
}

func TestShellRefusals(t *testing.T) {
	cases := []struct {
		typed string
		mods  key.Modifiers
		cwd   string
		want  string
	}{
		{typed: "!ls", mods: key.ModAlt, cwd: "/p", want: noticeShellAlt},
		{typed: "!   ", cwd: "/p", want: noticeShellText},
		{typed: "!ls", want: noticeShellNone},
	}
	for _, c := range cases {
		h := newHarness(t, Host{})
		h.view.openThread("").Thread.Cwd = c.cwd
		h.focus()
		h.typeText(c.typed)
		h.press(key.NameReturn, c.mods)
		if len(intentsOf[state.RunShell](h)) != 0 || len(intentsOf[state.Enqueue](h)) != 0 || !h.shows(c.want) || h.text() != c.typed {
			t.Fatalf("%q with %v: want refusal %q", c.typed, c.mods, c.want)
		}
	}
}

func TestShellExitAddsOneOutputChip(t *testing.T) {
	h := idleHarness(t, Host{})
	exit := 2
	h.view.runs[testKey] = []state.ShellRun{{Ticket: 99, N: 3, Command: "grep x", Phase: state.ShellExited, ExitCode: &exit, Output: "a ``` b\nc\n"}}
	h.frame()
	h.frame()
	chips := lastPut(h).Attachments
	if len(chips) != 1 || chips[0].Name != "[Shell #3 exit 2, 2 lines]" || chips[0].Text != "Output of `grep x` (exit 2):\n````\na ``` b\nc\n````" {
		t.Fatalf("chips = %+v", chips)
	}
	puts := len(intentsOf[state.PutDraft](h))
	h.frame()
	if len(intentsOf[state.PutDraft](h)) != puts || len(h.dock.draft.chips) != 1 {
		t.Fatal("one exited run must add exactly one chip")
	}
}

func TestShellResultsStayWithTheirThread(t *testing.T) {
	h := idleHarness(t, Host{})
	exit := 0
	other := state.DraftKey{Endpoint: "desktop", Window: testWindow, Thread: testThread}
	h.view.runs[other] = []state.ShellRun{{Ticket: 5, N: 1, Command: "ls", Phase: state.ShellExited, ExitCode: &exit, Output: "x"}}
	h.frame()
	if len(h.dock.draft.chips) != 0 || h.shows(shellRowsLabel) {
		t.Fatal("another endpoint's shell result entered this thread")
	}
}

func TestFollowUpQueueEditAndRemove(t *testing.T) {
	h := newHarness(t, Host{})
	h.view.openThread("turn-1")
	h.view.followUps[testKey] = []state.FollowUp{{ID: 7, Text: "first line\nsecond line"}, {ID: 9, Text: "other"}}
	h.focus()
	if !h.shows(queueLabel) || !h.shows("first line") || h.shows("first line\nsecond line") {
		t.Fatal("the queue must show each item's first line")
	}
	if n, ok := h.node("Edit"); !ok || n.Desc.Description != "first line" {
		t.Fatal("the Edit action must describe its item")
	}
	h.click("Edit")
	got := intentsOf[state.EditFollowUp](h)
	if !slices.Equal(got, []state.EditFollowUp{{Key: testKey, ID: 7}}) {
		t.Fatalf("edit = %+v", got)
	}
	h.view.followUps[testKey] = h.view.followUps[testKey][1:]
	h.view.drafts[testKey] = state.Draft{Text: "first line\nsecond line", Revision: 5}
	h.frame()
	if h.text() != "first line\nsecond line" {
		t.Fatalf("the edited follow-up must fill the field, got %q", h.text())
	}
	h.click("Remove")
	removed := intentsOf[state.RemoveFollowUp](h)
	if !slices.Equal(removed, []state.RemoveFollowUp{{Key: testKey, ID: 9}}) {
		t.Fatalf("remove = %+v", removed)
	}
}

func TestRefusedSendRestoresTheExactDraft(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("//keep the slash")
	h.press(key.NameReturn, 0)
	send := intentsOf[state.Send](h)[0]
	if send.Draft.Text != "/keep the slash" || h.text() != "" {
		t.Fatalf("send = %+v field %q", send, h.text())
	}
	h.view.drafts[testKey] = state.Draft{Text: "/keep the slash", Revision: send.Draft.Revision}
	h.frame()
	if h.text() != "//keep the slash" {
		t.Fatalf("a refused send must restore the typed text, got %q", h.text())
	}
}

func TestConsumedSendStaysClearAndLaterTypingSurvives(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("first")
	h.press(key.NameReturn, 0)
	send := intentsOf[state.Send](h)[0]
	h.typeText("second")
	h.view.drafts[testKey] = state.Draft{Revision: send.Draft.Revision}
	h.frame()
	if h.text() != "second" {
		t.Fatalf("late success cleared new input: %q", h.text())
	}
}

func TestOfflineEndpointBlocksSendAndKeepsText(t *testing.T) {
	h := idleHarness(t, Host{})
	h.view.threads[testThreadKey].Phase = client.PhaseOffline
	h.typeText("hello")
	h.press(key.NameReturn, 0)
	if len(intentsOf[state.Send](h)) != 0 || h.text() != "hello" || !h.shows("laptop is offline; reconnect before sending") {
		t.Fatal("an offline endpoint must block sending with a named reason")
	}
}

func TestBlankMessageIsRefused(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText(" \u3000 ")
	h.press(key.NameReturn, 0)
	if len(intentsOf[state.Send](h)) != 0 || !h.shows(noticeEmpty) {
		t.Fatal("a whitespace-only message must not send")
	}
}

func TestKeywordsLineFollowsCurrentText(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("fix the parser")
	h.wait(settleDelay)
	got := intentsOf[state.MatchKeywords](h)
	if len(got) != 1 || got[0] != (state.MatchKeywords{Key: testKey, Text: "fix the parser"}) {
		t.Fatalf("match = %+v", got)
	}
	h.view.keywords[testKey] = state.DraftKeywords{Text: "fix the parser", Matches: []rpc.KeywordMatch{{Word: "parser", ByteRange: rpc.ByteRange{Start: 8, End: 14}}}}
	h.frame()
	if !h.shows("Keywords: parser") {
		t.Fatal("draft keywords must show as words")
	}
	h.typeText("s")
	if h.shows("Keywords: parser") {
		t.Fatal("keywords for older text must disappear")
	}
}

func pngFile(t *testing.T, dir string) string {
	t.Helper()
	var buf bytes.Buffer
	if err := png.Encode(&buf, image.NewGray(image.Rect(0, 0, 4, 3))); err != nil {
		t.Fatal(err)
	}
	p := filepath.Join(dir, "shot.png")
	if err := os.WriteFile(p, buf.Bytes(), 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestAttachImageFromPicker(t *testing.T) {
	path := pngFile(t, t.TempDir())
	h := idleHarness(t, Host{PickImages: func(context.Context) ([]string, error) { return []string{path, path + ".missing"}, nil }})
	h.view.images[testThreadKey] = true
	h.click("Attach image")
	h.awaitWake()
	chips := lastPut(h).Attachments
	if len(chips) != 1 || chips[0].Kind != state.AttachmentImage || chips[0].Name != "shot.png" {
		t.Fatalf("chips = %+v", chips)
	}
	if !h.showsPrefix("Image · shot.png · ") || !h.showsPrefix("shot.png.missing: ") {
		t.Fatal("accepted images must show as chips and each failure must be reported")
	}
}

func TestModelWithoutImagesRefusesNewImage(t *testing.T) {
	h := idleHarness(t, Host{}).withClipboard(platform.ClipboardContent{Image: pngBytes(t, 2, 2), ImageType: "image/png"})
	h.view.images[testThreadKey] = false
	h.typeText("describe this")
	h.press("V", key.ModShortcut)
	h.awaitWake()
	if len(h.dock.draft.chips) != 0 || h.text() != "describe this" || !h.showsPrefix(pastedImageName+": "+string(errImagesRefused)) {
		t.Fatalf("chips %+v field %q", h.dock.draft.chips, h.text())
	}
}

func TestIncompatibleModelBlocksSendWithCorrection(t *testing.T) {
	h := idleHarness(t, Host{}).withClipboard(platform.ClipboardContent{Image: pngBytes(t, 2, 2), ImageType: "image/png"})
	h.view.images[testThreadKey] = true
	h.press("V", key.ModShortcut)
	h.awaitWake()
	h.view.images[testThreadKey] = false
	h.typeText("what is this")
	h.press(key.NameReturn, 0)
	if len(intentsOf[state.Send](h)) != 0 || len(h.dock.draft.chips) != 1 || !h.shows(string(errImagesRefused)) {
		t.Fatal("an incompatible model must block the send and keep the image")
	}
	h.click("Change model")
	if len(h.routes) != 1 || h.routes[0].Panel != "model" {
		t.Fatalf("routes = %+v", h.routes)
	}
}

func TestSendHoldsThePointOnlyInAnIdleUndecidedThread(t *testing.T) {
	h := idleHarness(t, Host{})
	if got := h.dock.primaryControl(h.env).variant; got != variantPoint {
		t.Fatalf("idle Send variant = %v, want the point", got)
	}
	h.env.Deciding = true
	if got := h.dock.primaryControl(h.env).variant; got == variantPoint {
		t.Fatal("Send kept the point while a request card holds it")
	}
	h.env.Deciding = false
	h.view.threads[testThreadKey].ActiveTurn = "turn-1"
	if c := h.dock.primaryControl(h.env); c.variant == variantPoint || c.label != "Steer" {
		t.Fatalf("running primary = %q %v, want a Steer without the point", c.label, c.variant)
	}
}

func TestModelChangeRechecksAttachedImages(t *testing.T) {
	h := idleHarness(t, Host{}).withClipboard(platform.ClipboardContent{Image: pngBytes(t, 2, 2), ImageType: "image/png"})
	h.view.images[testThreadKey] = true
	h.press("V", key.ModShortcut)
	h.awaitWake()
	if h.shows(string(errImagesRefused)) || h.shows("Change model") {
		t.Fatal("a model with image input must not show the refusal")
	}
	h.view.images[testThreadKey] = false
	h.frame()
	if !h.shows(string(errImagesRefused)) || !h.shows("Change model") || len(h.dock.draft.chips) != 1 {
		t.Fatal("switching to a model without image input must show the refusal and the correction while keeping the image")
	}
	h.view.images[testThreadKey] = true
	h.frame()
	if h.shows(string(errImagesRefused)) {
		t.Fatal("the refusal must clear once the model accepts images again")
	}
}

func TestHelpOpensKeyListInComposerContext(t *testing.T) {
	h := idleHarness(t, Host{})
	h.typeText("/help")
	h.press(key.NameReturn, 0)
	if len(h.routes) != 1 || h.routes[0] != (shell.Route{Panel: "help", Thread: testThreadKey, Context: "composer"}) {
		t.Fatalf("routes = %+v", h.routes)
	}
}
