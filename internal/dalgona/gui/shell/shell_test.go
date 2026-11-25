//go:build gui

package shell

import (
	"errors"
	"image"
	"strconv"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func manyRows(n int) []rpc.Thread {
	rows := make([]rpc.Thread, n)
	for i := range rows {
		rows[i] = summary("thread-"+strconv.Itoa(i), "Thread "+strconv.Itoa(i), "")
	}
	return rows
}

func regionsAt(t *testing.T, size image.Point) (*harness, map[string]image.Rectangle) {
	h := newHarness(t)
	h.size = size
	key := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := connectedView(manyRows(3)...)
	snap.Threads = []*state.ThreadView{openThread(key, "Thread 0")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.settle(v)
	return h, map[string]image.Rectangle{
		navigationLabel: h.region(navigationLabel),
		transcriptLabel: h.region(transcriptLabel),
		dockLabel:       h.region(dockLabel),
	}
}

func TestWideSidebarIsAFullHeightColumnWithinItsWidths(t *testing.T) {
	size := image.Pt(int(theme.ShellStackWidth)+1, 800)
	_, r := regionsAt(t, size)
	nav := r[navigationLabel]
	if want := image.Rect(0, 0, nav.Dx(), size.Y); nav != want {
		t.Errorf("sidebar spans %v, want the left column %v", nav, want)
	}
	if w := nav.Dx(); w < int(theme.SidebarMinWidth) || w > int(theme.SidebarMaxWidth) {
		t.Errorf("sidebar is %d wide, want between %v and %v", w, theme.SidebarMinWidth, theme.SidebarMaxWidth)
	}
}

func TestWideWorkspaceKeepsHeaderTranscriptDockOrder(t *testing.T) {
	size := image.Pt(1280, 800)
	_, r := regionsAt(t, size)
	transcript, dock := r[transcriptLabel], r[dockLabel]
	if dock.Max.Y != size.Y || transcript.Max.Y > dock.Min.Y || transcript.Min.Y == 0 || transcript.Dy() == 0 || dock.Dy() == 0 {
		t.Errorf("transcript %v and dock %v must follow the header and end at the window bottom %d", transcript, dock, size.Y)
	}
}

func TestStackedNavigationSitsAboveTheWorkspaceWithinItsCap(t *testing.T) {
	size := image.Pt(int(theme.ShellStackWidth), 900)
	_, r := regionsAt(t, size)
	nav := r[navigationLabel]
	if nav.Min.Y != 0 || nav.Dx() != size.X {
		t.Errorf("stacked navigation %v must span the top of a %v window", nav, size)
	}
	if nav.Dy() > int(theme.ThreadListStackCap) {
		t.Errorf("stacked navigation is %d tall, above the %v cap", nav.Dy(), theme.ThreadListStackCap)
	}
	if r[transcriptLabel].Min.Y < nav.Max.Y {
		t.Errorf("transcript %v starts above the navigation end %d", r[transcriptLabel], nav.Max.Y)
	}
	if dock := r[dockLabel]; dock.Max.Y != size.Y || dock.Dx() != size.X {
		t.Errorf("stacked dock %v must span the window bottom", dock)
	}
}

func TestStackedTallDockKeepsIdleClearance(t *testing.T) {
	h := newHarness(t)
	h.size = image.Pt(600, 400)
	h.composer.height = 1000
	key := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := connectedView(manyRows(1)...)
	snap.Threads = []*state.ThreadView{openThread(key, "Thread 0")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.settle(v)
	if dock := h.region(dockLabel); dock.Min.Y < int(theme.IdleDockClearance) {
		t.Errorf("dock %v leaves less than %v above it", dock, theme.IdleDockClearance)
	}
}

func TestLaunchShowsEmptyWorkspaceWithoutOpeningAThread(t *testing.T) {
	h := newHarness(t)
	v := connectedView(manyRows(2)...).View()
	h.settle(v)
	if opens := submitted[state.OpenView](h.take()); len(opens) != 0 {
		t.Fatalf("launch opened %v without a saved reference", opens)
	}
	for _, want := range []string{"Pick a thread or start a new one", "Earlier threads appear in the list."} {
		if !h.has(want) {
			t.Errorf("empty workspace lacks %q: %v", want, h.labels())
		}
	}
	if _, ok := h.button(startLabel); !ok {
		t.Error("empty workspace offers no Start thread")
	}
}

func TestEmptyWorkspaceOffersRecoveryWhenNoEndpointIsLive(t *testing.T) {
	h := newHarness(t)
	snap := state.Snapshot{Endpoints: []state.EndpointView{endpoint(local, "local", client.PhaseOffline), endpoint(remote, "lab", client.PhaseIdle)}}
	v := snap.View()
	h.settle(v)
	if h.has("Earlier threads appear in the list.") {
		t.Error("an all-offline window still claims earlier threads appear")
	}
	h.take()
	h.click(v, "Connect lab")
	got := submitted[state.Connect](h.take())
	if len(got) != 1 || got[0].Endpoint != remote {
		t.Fatalf("Connect lab submitted %v, want one Connect for %q", got, remote)
	}
}

func TestSelectingARowOpensItOnItsOwnEndpointWithoutReconnecting(t *testing.T) {
	h := newHarness(t)
	snap := connectedView(manyRows(2)...)
	snap.Endpoints = append(snap.Endpoints, endpoint(remote, "lab", client.PhaseConnected))
	snap.Lanes[remote] = loaded(summary("thread-0", "Remote zero", ""))
	v := snap.View()
	h.settle(v)
	h.take()
	h.click(v, "Remote zero")
	h.click(v, "Thread 1")
	intents := h.take()
	if len(submitted[state.Connect](intents)) != 0 {
		t.Fatalf("selecting threads reconnected an endpoint: %v", intents)
	}
	want := []state.Intent{
		state.OpenView{Window: testWindow, Thread: state.ThreadKey{Endpoint: remote, Thread: "thread-0"}},
		state.OpenView{Window: testWindow, Thread: state.ThreadKey{Endpoint: local, Thread: "thread-1"}},
		state.CloseView{Window: testWindow, Thread: state.ThreadKey{Endpoint: remote, Thread: "thread-0"}},
	}
	for _, w := range want {
		if !containsIntent(intents, w) {
			t.Errorf("missing %#v in %#v", w, intents)
		}
	}
}

func containsIntent(intents []state.Intent, want state.Intent) bool {
	for _, i := range intents {
		if i == want {
			return true
		}
	}
	return false
}

func TestWindowTitleNamesThreadAndState(t *testing.T) {
	h := newHarness(t)
	key := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := connectedView(summary("thread-0", "Fix the parser", ""))
	snap.Threads = []*state.ThreadView{openThread(key, "Fix the parser", state.TurnView{ID: "t1", Status: "completed"})}
	v := snap.View()
	fr := Frame{Theme: theme.Night, View: v}
	if got := h.shell.WindowTitle(fr); got != "dalgona" {
		t.Errorf("title without a thread = %q", got)
	}
	h.settle(v)
	h.click(v, "Fix the parser")
	if got, want := h.shell.WindowTitle(fr), "dalgona · Fix the parser · Done"; got != want {
		t.Errorf("title = %q, want %q", got, want)
	}
}

func queuedSnapshot() (state.Snapshot, state.ThreadKey, state.ThreadKey) {
	from := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	to := state.ThreadKey{Endpoint: local, Thread: "thread-1"}
	snap := connectedView(manyRows(2)...)
	snap.Threads = []*state.ThreadView{openThread(from, "Thread 0")}
	snap.FollowUps = map[state.DraftKey][]state.FollowUp{{Endpoint: local, Window: testWindow, Thread: "thread-0"}: {{ID: 1, Text: "next"}, {ID: 2, Text: "after"}}}
	return snap, from, to
}

func TestOpeningAnotherThreadWithQueuedFollowUpsAsksFirst(t *testing.T) {
	h := newHarness(t)
	snap, _, _ := queuedSnapshot()
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.take()
	h.click(v, "Thread 1")
	if opens := submitted[state.OpenView](h.take()); len(opens) != 0 {
		t.Fatalf("opened %v before the discard decision", opens)
	}
	if !h.has("2 queued follow-ups") {
		t.Errorf("decision does not show the queued count: %v", h.labels())
	}
	h.click(v, cancelLabel)
	if intents := h.take(); len(intents) != 0 || h.shell.Open().Thread != "thread-0" {
		t.Fatalf("Cancel submitted %v and left %v open", intents, h.shell.Open())
	}
}

func TestConfirmedOpenDiscardsTheQueueOnlyAfterTheNewThreadOpens(t *testing.T) {
	h := newHarness(t)
	snap, from, to := queuedSnapshot()
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.click(v, "Thread 1")
	h.take()
	h.click(v, discardLabel)
	if got := h.take(); len(submitted[state.DiscardFollowUps](got)) != 0 || !containsIntent(got, state.OpenView{Window: testWindow, Thread: to}) {
		t.Fatalf("confirm submitted %v; want only the open of %v", got, to)
	}
	opening := openThread(to, "Thread 1")
	opening.Opening, opening.HistoryID = true, ""
	snap.Threads = append(snap.Threads, opening)
	h.frame(snap.View())
	if got := h.take(); len(got) != 0 {
		t.Fatalf("submitted %v while the new thread was still opening", got)
	}
	snap.Threads[1] = openThread(to, "Thread 1")
	h.frame(snap.View())
	got := h.take()
	if !containsIntent(got, state.DiscardFollowUps{Key: state.DraftKey{Endpoint: local, Window: testWindow, Thread: from.Thread}}) || !containsIntent(got, state.CloseView{Window: testWindow, Thread: from}) {
		t.Fatalf("after the open succeeded got %v, want the old queue discarded and its view closed", got)
	}
}

func TestFailedOpenKeepsTheOriginalQueueAndThread(t *testing.T) {
	h := newHarness(t)
	snap, from, to := queuedSnapshot()
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.click(v, "Thread 1")
	h.click(v, discardLabel)
	h.take()
	failed := openThread(to, "Thread 1")
	failed.Err = errors.New("thread/resume: no such thread")
	snap.Threads = append(snap.Threads, failed)
	h.settle(snap.View())
	got := h.take()
	if len(submitted[state.DiscardFollowUps](got)) != 0 {
		t.Fatalf("a failed open discarded the queue: %v", got)
	}
	if h.shell.Open() != from {
		t.Errorf("open thread = %v, want the original %v", h.shell.Open(), from)
	}
	if !h.has("did not open") {
		t.Errorf("no failure notice: %v", h.labels())
	}
}

func TestChildNavigationKeepsABackRoute(t *testing.T) {
	h := newHarness(t)
	parent := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	child := state.ThreadKey{Endpoint: local, Thread: "child-1"}
	snap := connectedView(manyRows(1)...)
	snap.Threads = []*state.ThreadView{openThread(parent, "Thread 0"), openThread(child, "Child task")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.transcript.envs[len(h.transcript.envs)-1].Navigate(child)
	h.settle(v)
	if h.shell.Open() != child {
		t.Fatalf("Navigate left %v open, want %v", h.shell.Open(), child)
	}
	h.take()
	h.click(v, backPrefix+"Thread 0")
	if h.shell.Open() != parent || !containsIntent(h.take(), state.OpenView{Window: testWindow, Thread: parent}) {
		t.Fatalf("Back opened %v, want the parent %v", h.shell.Open(), parent)
	}
}

func TestPanelRoutesReachTheirPanelAndBlockTheShell(t *testing.T) {
	h := newHarness(t)
	v := connectedView(manyRows(2)...).View()
	h.settle(v)
	h.composer.envs[len(h.composer.envs)-1].Open(Route{Panel: "model"})
	h.settle(v)
	if got := h.panels["model"].opened; len(got) != 1 || got[0].Panel != "model" {
		t.Fatalf("model panel opened with %v", got)
	}
	if !h.panels["model"].env.Modal {
		t.Error("Env.Modal is false while a panel is open")
	}
	h.take()
	row := h.region("Thread 1")
	h.clickAt(row)
	h.frame(v)
	if got := submitted[state.OpenView](h.take()); len(got) != 0 {
		t.Fatalf("a click behind the panel opened %v", got)
	}
	h.click(v, "Close model")
	h.frame(v)
	if _, ok := h.button("Close model"); ok {
		t.Error("the panel stayed after Close")
	}
}

func TestQuestionMarkOpensTheKeyListOutsideTheComposer(t *testing.T) {
	h := newHarness(t)
	v := connectedView(manyRows(1)...).View()
	h.settle(v)
	h.press(v, "?", key.ModShift)
	h.frame(v)
	if got := h.panels["help"].opened; len(got) != 1 {
		t.Fatalf("? opened help %d times, want once", len(got))
	}
}

func TestQuestionMarkInTheComposerStaysText(t *testing.T) {
	h := newHarness(t)
	key0 := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := connectedView(manyRows(1)...)
	snap.Threads = []*state.ThreadView{openThread(key0, "Thread 0")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.focus(v, h.composer.FocusTag())
	h.press(v, "?", key.ModShift)
	if got := h.panels["help"].opened; len(got) != 0 {
		t.Fatalf("? inside the composer opened help %v", got)
	}
}

func TestSettlersRunEveryFrameWithoutAPanel(t *testing.T) {
	h := newHarness(t)
	v := connectedView().View()
	h.settle(v)
	h.frame(v)
	if got := h.settler.windows; len(got) != 3 || got[0] != testWindow {
		t.Fatalf("settler ran for %v, want three frames of %q", got, testWindow)
	}
}

func TestRequestCardReadsKeysBeforeTheComposer(t *testing.T) {
	h := newHarness(t)
	h.request.showing = true
	v := connectedView().View()
	h.settle(v)
	for i, first := range h.request.before {
		if !first {
			t.Fatalf("frame %d laid out the composer before the request card", i)
		}
	}
	if env := h.composer.envs[len(h.composer.envs)-1]; !env.Deciding {
		t.Error("Env.Deciding is false while the request card holds the point")
	}
}
