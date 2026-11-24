//go:build gui

package shell

import (
	"errors"
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func noticesView(notices ...state.Notice) state.Snapshot {
	snap := connectedView(summary("thread-0", "Thread 0", ""))
	snap.Notices = notices
	return snap
}

func TestChannelsExistBeforeAnyNotice(t *testing.T) {
	h := newHarness(t)
	h.settle(connectedView().View())
	for _, want := range []string{errorsLabel, statusLabel} {
		if !h.has(want) {
			t.Errorf("no %q channel before notices: %v", want, h.labels())
		}
	}
}

func TestStackHoldsFiveAndDropsTheOldest(t *testing.T) {
	h := newHarness(t)
	var notices []state.Notice
	for i := range 7 {
		notices = append(notices, state.Notice{ID: uint64(i + 1), Endpoint: local, Text: fmt.Sprintf("notice %d", i), Tone: state.NoticeStatus})
	}
	h.settle(noticesView(notices...).View())
	for i := range 7 {
		shown := h.has(fmt.Sprintf("notice %d", i))
		if want := i >= 2; shown != want {
			t.Errorf("notice %d shown=%v, want %v", i, shown, want)
		}
	}
}

func TestIdenticalTextKeepsOneInstance(t *testing.T) {
	h := newHarness(t)
	h.settle(noticesView(state.Notice{ID: 1, Text: "same"}, state.Notice{ID: 2, Text: "same"}).View())
	count := 0
	for _, n := range h.nodes {
		if n.Desc.Label == "same" {
			count++
		}
	}
	if count != 1 {
		t.Fatalf("identical notices drawn %d times", count)
	}
}

func TestDismissRemovesTheStateNoticeAndRestoresFocus(t *testing.T) {
	h := newHarness(t)
	key := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	snap := noticesView(state.Notice{ID: 9, Endpoint: local, Text: "The message changed before it was sent; send it again", Tone: state.NoticeError})
	snap.Threads = []*state.ThreadView{openThread(key, "Thread 0")}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	h.take()
	h.click(v, "Dismiss")
	h.frame(v)
	if got := submitted[state.DismissNotice](h.take()); len(got) != 1 || got[0].ID != 9 {
		t.Fatalf("Dismiss submitted %v", got)
	}
	if !h.router.Source().Focused(h.composer.FocusTag()) {
		t.Error("Dismiss did not return focus to the composer")
	}
}

func mutationView(mu state.MutationView, phase client.Phase) *state.View {
	snap := connectedView()
	snap.Endpoints = []state.EndpointView{endpoint(local, "local", phase)}
	snap.Mutations = []state.MutationView{mu}
	return snap.View()
}

func TestFailedSendRetriesWhileConnectedAndReconnectsWhileOffline(t *testing.T) {
	mu := state.MutationView{OperationID: "op-1", Key: state.DraftKey{Endpoint: local, Window: testWindow, Thread: "thread-0"}, Method: "turn/start", State: state.MutationFailed, Err: errors.New("provider refused"), Payload: state.Draft{Text: "hello"}}
	h := newHarness(t)
	v := mutationView(mu, client.PhaseConnected)
	h.settle(v)
	if !h.has("Could not send the message on local: provider refused") {
		t.Fatalf("failure notice lacks its action and cause: %v", h.labels())
	}
	h.take()
	h.click(v, "Retry")
	if got := submitted[state.Retry](h.take()); len(got) != 1 || got[0].OperationID != "op-1" {
		t.Fatalf("Retry submitted %v", got)
	}
	h.click(v, "Restore message")
	if got := submitted[state.Restore](h.take()); len(got) != 1 {
		t.Fatalf("Restore submitted %v", got)
	}
	off := newHarness(t)
	ov := mutationView(mu, client.PhaseOffline)
	off.settle(ov)
	if _, ok := off.button("Retry"); ok {
		t.Error("an offline failure offers Retry")
	}
	off.take()
	off.click(ov, "Reconnect")
	if got := submitted[state.Connect](off.take()); len(got) != 1 || got[0].Endpoint != local {
		t.Fatalf("Reconnect submitted %v", got)
	}
}

func TestUnknownOutcomeOffersCheckNotRetry(t *testing.T) {
	mu := state.MutationView{OperationID: "op-2", Key: state.DraftKey{Endpoint: local, Window: testWindow}, Method: "turn/start", State: state.MutationUnknown, Receipt: true, Payload: state.Draft{Text: "x"}}
	h := newHarness(t)
	v := mutationView(mu, client.PhaseConnected)
	h.settle(v)
	for _, absent := range []string{"Retry", "Restore message"} {
		if _, ok := h.button(absent); ok {
			t.Errorf("an uncertain write offers %q", absent)
		}
	}
	h.take()
	h.click(v, "Check")
	if got := submitted[state.Check](h.take()); len(got) != 1 || got[0].OperationID != "op-2" {
		t.Fatalf("Check submitted %v", got)
	}
}

func TestAnotherWindowsMutationStaysThere(t *testing.T) {
	mu := state.MutationView{OperationID: "op-3", Key: state.DraftKey{Endpoint: local, Window: "window-2"}, Method: "turn/start", State: state.MutationFailed, Err: errors.New("boom")}
	h := newHarness(t)
	h.settle(mutationView(mu, client.PhaseConnected))
	if h.has("boom") {
		t.Fatal("this window shows another window's failed send")
	}
}

func TestConnectionFailureNamesTheEndpointAndOffersReconnect(t *testing.T) {
	h := newHarness(t)
	failed := endpoint(remote, "lab", client.PhaseFailed)
	failed.Err = errors.New("peer certificate is not trusted")
	snap := connectedView(summary("thread-0", "Thread 0", ""))
	snap.Endpoints = append(snap.Endpoints, failed)
	v := snap.View()
	h.settle(v)
	if !h.has("Could not connect to lab: peer certificate is not trusted") {
		t.Fatalf("no endpoint-named connection notice: %v", h.labels())
	}
	if h.has("dalgona web") {
		t.Error("a native notice points at dalgona web")
	}
	h.take()
	h.click(v, "Reconnect")
	if got := submitted[state.Connect](h.take()); len(got) != 1 || got[0].Endpoint != remote {
		t.Fatalf("Reconnect submitted %v", got)
	}
}

func TestRequestsElsewhereShareOneNoticeWithOpenThread(t *testing.T) {
	h := newHarness(t)
	here := state.ThreadKey{Endpoint: local, Thread: "thread-0"}
	there := state.ThreadKey{Endpoint: remote, Thread: "thread-9"}
	other := openThread(there, "Deploy")
	other.Requests = []state.RequestView{
		{Key: state.RequestKey{Thread: there, Request: "a"}, Method: "item/commandExecution/requestApproval", Actionable: true},
		{Key: state.RequestKey{Thread: there, Request: "b"}, Method: "item/tool/requestUserInput", Actionable: true},
	}
	snap := connectedView(summary("thread-0", "Thread 0", ""))
	snap.Endpoints = append(snap.Endpoints, endpoint(remote, "lab", client.PhaseConnected))
	snap.Threads = []*state.ThreadView{openThread(here, "Thread 0"), other}
	v := snap.View()
	h.settle(v)
	h.click(v, "Thread 0")
	if !h.has("2 requests waiting on another thread: Deploy on lab.") {
		t.Fatalf("no shared waiting notice: %v", h.labels())
	}
	h.take()
	h.click(v, openThreadAct)
	if got := submitted[state.OpenView](h.take()); len(got) != 1 || got[0].Thread != there {
		t.Fatalf("Open thread submitted %v, want %v", got, there)
	}
}

func TestNoticesAnnounceOnceThroughTheirChannel(t *testing.T) {
	h := newHarness(t)
	v := noticesView(state.Notice{ID: 1, Text: "queue full", Tone: state.NoticeError}).View()
	h.settle(v)
	h.settle(v)
	count := 0
	for _, a := range h.announced {
		if a.Text == "queue full" {
			count++
		}
	}
	if count != 1 || !h.has(errorsLabel+": queue full") {
		t.Fatalf("announced %d times; channels %v", count, h.labels())
	}
}

func TestRepeatedAnnouncementReachesTheBridgeOnce(t *testing.T) {
	h := newHarness(t)
	v := connectedView().View()
	h.settle(v)
	env := h.composer.envs[len(h.composer.envs)-1]
	done := platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Done in Release"}
	for range 3 {
		env.Announce(done)
		h.frame(v)
	}
	env.Announce(platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Done in Deploy"})
	count := 0
	for _, a := range h.announced {
		if a == done {
			count++
		}
	}
	if count != 1 || h.announced[len(h.announced)-1].Text != "Done in Deploy" {
		t.Fatalf("bridge received %v", h.announced)
	}
}
