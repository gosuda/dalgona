//go:build gui

package shell

import (
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func twoLanes(localLane, remoteLane state.LaneView, remotePhase client.Phase) state.Snapshot {
	return state.Snapshot{
		Endpoints: []state.EndpointView{endpoint(local, "local", client.PhaseConnected), endpoint(remote, "lab", remotePhase)},
		Lanes:     map[client.EndpointID]state.LaneView{local: localLane, remote: remoteLane},
	}
}

func TestEqualThreadIDsOnTwoEndpointsAreDistinctRows(t *testing.T) {
	h := newHarness(t)
	v := twoLanes(loaded(summary("same", "Local same", "")), loaded(summary("same", "Remote same", "")), client.PhaseConnected).View()
	h.settle(v)
	h.take()
	h.click(v, "Remote same")
	h.click(v, "Local same")
	opens := submitted[state.OpenView](h.take())
	want := []state.ThreadKey{{Endpoint: remote, Thread: "same"}, {Endpoint: local, Thread: "same"}}
	if len(opens) != 2 || opens[0].Thread != want[0] || opens[1].Thread != want[1] {
		t.Fatalf("opened %v, want %v", opens, want)
	}
}

func TestEveryLaneHeaderShowsItsLabelAndPhaseWord(t *testing.T) {
	words := map[client.Phase]string{
		client.PhaseIdle: "Idle", client.PhaseConnecting: "Connecting", client.PhaseConnected: "Connected",
		client.PhaseReconnecting: "Reconnecting", client.PhaseOffline: "Offline", client.PhaseIncompatible: "Incompatible", client.PhaseFailed: "Failed",
	}
	for phase, word := range words {
		h := newHarness(t)
		h.settle(twoLanes(loaded(), state.LaneView{}, phase).View())
		if !h.has("lab, " + word) {
			t.Errorf("phase %v: no lane header %q in %v", phase, "lab, "+word, h.labels())
		}
	}
}

func TestOfflineLaneKeepsRowsAndOffersConnect(t *testing.T) {
	h := newHarness(t)
	v := twoLanes(loaded(summary("l1", "Local one", "")), loaded(summary("r1", "Cached remote", "")), client.PhaseOffline).View()
	h.settle(v)
	if !h.has(unavailableText) || !h.has("Cached remote") {
		t.Fatalf("offline lane lost its rows or its unavailable text: %v", h.labels())
	}
	if h.has("No threads yet") {
		t.Error("a disconnected lane claims that no threads exist")
	}
	h.take()
	h.click(v, connectLabel)
	if got := submitted[state.Connect](h.take()); len(got) != 1 || got[0].Endpoint != remote {
		t.Fatalf("Connect submitted %v, want one for %q", got, remote)
	}
}

func TestIdleLaneShowsNoLoss(t *testing.T) {
	h := newHarness(t)
	h.settle(twoLanes(loaded(), loaded(summary("r1", "Idle remote", "")), client.PhaseIdle).View())
	if h.has(unavailableText) || h.has("lab is offline") {
		t.Errorf("idle lane shows a loss: %v", h.labels())
	}
	if !h.has("Idle remote") {
		t.Error("idle lane dropped its cached rows")
	}
}

func TestEmptyConnectedLaneOffersStartOnThatEndpoint(t *testing.T) {
	h := newHarness(t)
	v := twoLanes(loaded(summary("l1", "Local one", "")), loaded(), client.PhaseConnected).View()
	h.settle(v)
	if !h.has("No threads yet") {
		t.Fatalf("empty lane text missing: %v", h.labels())
	}
	for _, n := range h.nodes {
		if n.Desc.Label == startLabel && n.Desc.Description == "on lab" {
			h.clickAt(n.Desc.Bounds)
		}
	}
	h.frame(v)
	opened := h.panels["new"].opened
	if len(opened) != 1 || opened[0].Thread.Endpoint != remote {
		t.Fatalf("lane Start thread opened %v, want one new-thread panel running on %q", opened, remote)
	}
}

func TestLoadMoreAppendsThroughTheLaneAndStaysFocusableWhileLoading(t *testing.T) {
	h := newHarness(t)
	lane := loaded(summary("r1", "Remote one", ""))
	lane.More = true
	v := twoLanes(loaded(), lane, client.PhaseConnected).View()
	h.settle(v)
	h.take()
	h.click(v, loadMoreLabel)
	if got := submitted[state.LoadThreads](h.take()); len(got) != 1 || got[0].Endpoint != remote {
		t.Fatalf("Load more submitted %v, want one LoadThreads for %q", got, remote)
	}
	lane.Loading = true
	busy := twoLanes(loaded(), lane, client.PhaseConnected).View()
	h.settle(busy)
	n, ok := h.button(loadMoreLabel)
	if !ok || !n.Desc.Disabled {
		t.Fatalf("loading Load more present=%v disabled=%v, want present and busy", ok, n.Desc.Disabled)
	}
	h.click(busy, loadMoreLabel)
	if got := h.take(); len(got) != 0 {
		t.Fatalf("activating a busy Load more submitted %v", got)
	}
}

func TestFailedPageKeepsRowsAndRetriesOnItsEndpoint(t *testing.T) {
	h := newHarness(t)
	lane := loaded(summary("r1", "Remote one", ""))
	lane.Err = errors.New("thread/list timed out")
	v := twoLanes(loaded(), lane, client.PhaseConnected).View()
	h.settle(v)
	if !h.has("Remote one") || !h.has("Could not load threads: thread/list timed out") {
		t.Fatalf("failed page lost rows or its cause: %v", h.labels())
	}
	h.take()
	h.click(v, retryLabel)
	if got := submitted[state.LoadThreads](h.take()); len(got) != 1 || got[0].Endpoint != remote {
		t.Fatalf("Retry submitted %v", got)
	}
}

func TestTitleFallsBackToPreviewThenUntitled(t *testing.T) {
	h := newHarness(t)
	h.settle(connectedView(summary("a", "", "first prompt text"), summary("b", "", "")).View())
	for _, want := range []string{"first prompt text", "Untitled thread"} {
		if _, ok := h.button(want); !ok {
			t.Errorf("no row titled %q: %v", want, h.labels())
		}
	}
}

func TestSelectedRowIsMarkedSelected(t *testing.T) {
	h := newHarness(t)
	key := state.ThreadKey{Endpoint: local, Thread: "thread-1"}
	snap := connectedView(manyRows(2)...)
	snap.Threads = []*state.ThreadView{openThread(key, "Thread 1")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 1")
	one, _ := h.button("Thread 1")
	zero, _ := h.button("Thread 0")
	if !one.Desc.Selected || zero.Desc.Selected {
		t.Fatalf("selected flags: Thread 1=%v Thread 0=%v", one.Desc.Selected, zero.Desc.Selected)
	}
}

func TestOnlyTheLocalLaneLoadsAtLaunchAndOnlyOnce(t *testing.T) {
	h := newHarness(t)
	snap := state.Snapshot{Endpoints: []state.EndpointView{endpoint(local, "local", client.PhaseIdle), endpoint(remote, "lab", client.PhaseIdle)}}
	v := snap.View()
	h.settle(v)
	h.frame(v)
	got := submitted[state.LoadThreads](h.take())
	if len(got) != 1 || got[0].Endpoint != local {
		t.Fatalf("launch loads %v, want exactly one LoadThreads for the local lane", got)
	}
	if _, ok := h.button(loadLabel); !ok {
		t.Error("the idle remote lane offers no explicit Load threads")
	}
}

func TestRowStateWordsFollowThreadStatus(t *testing.T) {
	h := newHarness(t)
	waiting := summary("w", "Needs input", "")
	waiting.Status = rpc.ThreadStatus{Type: "active", ActiveFlags: []string{"waitingOnUserInput"}}
	broken := summary("f", "Broke", "")
	broken.Status = rpc.ThreadStatus{Type: "systemError"}
	h.settle(connectedView(waiting, broken, summary("r", "Fresh", "")).View())
	for _, want := range []string{"Waiting · needs an answer", "Failed", "Ready"} {
		if !h.has(want) {
			t.Errorf("no row state %q in %v", want, h.labels())
		}
	}
}
