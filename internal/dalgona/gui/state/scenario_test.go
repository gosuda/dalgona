package state

import (
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (w *modelWorld) serveCalls() {
	for len(w.calls) > 0 {
		call := w.calls[0]
		w.calls = w.calls[1:]
		w.results = append(w.results, w.serve(call))
	}
}

func (w *modelWorld) deliverResults() {
	for len(w.results) > 0 {
		r := w.results[0]
		w.results = w.results[1:]
		w.deliverResult(r)
	}
}

func (w *modelWorld) deliverOutbox(d *fakeDaemon) {
	for w.deliverNext(d) {
	}
	for len(d.inflight) > 0 {
		w.deliverRequest(d, 0)
	}
}

func (w *modelWorld) deliverInOrder(d *fakeDaemon, responseFirst bool) {
	if responseFirst {
		w.deliverResults()
		w.deliverOutbox(d)
		return
	}
	w.deliverOutbox(d)
	w.deliverResults()
}

func (w *modelWorld) deltas(d *fakeDaemon, th *truthThread, texts ...string) {
	for _, text := range texts {
		w.appendDelta(d, th, []byte(text))
	}
}

func openedSubscribed(t *testing.T, seed uint64) (*modelWorld, *fakeDaemon, ThreadKey) {
	w := newModelWorld(t, seed, &modelCoverage{})
	d := w.daemon("local")
	key := ThreadKey{Endpoint: d.id, Thread: "t1"}
	w.handle(OpenView{Window: "w1", Thread: key})
	if !d.want {
		t.Fatal("opening a view on an idle endpoint did not request a connection")
	}
	w.connect(d)
	d.subs[key.Thread] = true
	return w, d, key
}

func TestSnapshotCutsSplitPendingSameItemDeltasExactlyOnce(t *testing.T) {
	for _, responseFirst := range []bool{true, false} {
		w, d, key := openedSubscribed(t, 7)
		th := d.threads[key.Thread]
		w.startTurn(d, th)
		w.startItem(d, th)
		w.deltas(d, th, "a", "가", "😀")
		w.serveCalls()
		w.deltas(d, th, "é", "\n")
		w.deliverInOrder(d, responseFirst)
		w.settle()
		w.verify()

		w.deltas(d, th, "b", "日本")
		w.handle(Refresh{Thread: key})
		w.deltas(d, th, "z")
		w.serveCalls()
		w.deltas(d, th, " ", "끝")
		w.deliverInOrder(d, responseFirst)
		w.settle()
		w.verify()
		if w.seen.replacements != 1 {
			t.Fatalf("the refresh used %d in-slot replacements, want 1", w.seen.replacements)
		}
		if w.seen.coalesced == 0 {
			t.Fatal("no coalesced same-item envelope reached the client")
		}
	}
}

func TestDelayedSnapshotsForTwoThreadsKeepTheirOwnCuts(t *testing.T) {
	w, d, first := openedSubscribed(t, 11)
	second := ThreadKey{Endpoint: d.id, Thread: "t2"}
	w.handle(OpenView{Window: "w2", Thread: second})
	d.subs[second.Thread] = true
	one, two := d.threads[first.Thread], d.threads[second.Thread]
	w.startTurn(d, one)
	w.startTurn(d, two)
	w.startItem(d, one)
	w.startItem(d, two)
	w.deltas(d, one, "one-a")
	w.deltas(d, two, "two-a")
	opens := w.takeCalls(methodOpen)
	firstOpen, secondOpen := opens[0], opens[1]
	w.results = append(w.results, w.serve(firstOpen))
	w.deltas(d, two, "two-b")
	w.deltas(d, one, "one-b")
	w.results = append(w.results, w.serve(secondOpen))
	w.deltas(d, one, "one-c")
	w.deltas(d, two, "two-c")
	w.deliverOutbox(d)
	w.results[0], w.results[1] = w.results[1], w.results[0]
	w.deliverResults()
	w.settle()
	w.verify()
}

func (w *modelWorld) takeCalls(method string) []effect {
	var taken []effect
	w.calls = slices.DeleteFunc(w.calls, func(e effect) bool {
		call, ok := e.(callEffect)
		if ok && call.method == method {
			taken = append(taken, e)
		}
		return ok && call.method == method
	})
	return taken
}

func TestStraddlingEnvelopeForcesResyncWithoutGuessingASuffix(t *testing.T) {
	for _, responseFirst := range []bool{true, false} {
		w, d, key := openedSubscribed(t, 13)
		th := d.threads[key.Thread]
		w.startTurn(d, th)
		w.startItem(d, th)
		w.deltas(d, th, "a", "b")
		w.serveCalls()
		d.outbox[len(d.outbox)-1].sealed = false
		w.deltas(d, th, "suffix")
		w.deliverInOrder(d, responseFirst)
		if w.resyncs != 1 {
			t.Fatalf("a straddling envelope caused %d resyncs, want 1", w.resyncs)
		}
		tv, ok := w.m.view().Thread(key)
		if !ok || tv.Live || !tv.Offline {
			t.Fatalf("after a broken stream the view is %+v, want frozen and offline", tv)
		}
		expectNoTail(t, tv)
	}
}

func expectNoTail(t *testing.T, tv *ThreadView) {
	t.Helper()
	for _, turn := range tv.Turns {
		for _, item := range turn.Items {
			if len(item.Tail) != 0 {
				t.Fatalf("item %s applied %q from the straddling envelope", item.Key.Item, item.Tail)
			}
		}
	}
}

func TestSameThreadIDOnTwoEndpointsStaysDistinct(t *testing.T) {
	w, local, key := openedSubscribed(t, 17)
	box := w.daemon("box")
	other := ThreadKey{Endpoint: box.id, Thread: key.Thread}
	w.handle(OpenView{Window: "w1", Thread: other})
	w.connect(box)
	box.subs[other.Thread] = true
	w.settle()
	for _, d := range []*fakeDaemon{local, box} {
		th := d.threads[key.Thread]
		w.startTurn(d, th)
		w.startItem(d, th)
		w.deltas(d, th, string(d.id))
	}
	w.settle()
	w.verify()
	v := w.m.view()
	a, _ := v.Thread(key)
	b, _ := v.Thread(other)
	if w.resolveText(a.Turns[0].Items[0]) != "local" || w.resolveText(b.Turns[0].Items[0]) != "box" {
		t.Fatalf("thread %s mixed endpoints: local reads %q, box reads %q", key.Thread, w.resolveText(a.Turns[0].Items[0]), w.resolveText(b.Turns[0].Items[0]))
	}
}

func TestPlanAdvisorChildPatchAndWarningEventsReachTheView(t *testing.T) {
	w, d, key := openedSubscribed(t, 19)
	w.settle()
	th := d.threads[key.Thread]
	th.pending = append(th.pending, w.request(d, th))
	w.startTurn(d, th)
	w.startItem(d, th)
	turn, item := th.active.id, th.active.open.id
	plan := []rpc.TurnPlanStep{{Step: "read", Status: "completed"}, {Step: "write", Status: turnInProgress}}
	events := []struct {
		method string
		params any
	}{
		{"turn/plan/updated", rpc.TurnPlanUpdatedNotification{ThreadID: key.Thread, TurnID: turn, Plan: plan}},
		{"dalgona/turn/effort", rpc.TurnEffortNotification{ThreadID: key.Thread, TurnID: turn, Effort: "high"}},
		{"item/fileChange/patchUpdated", patchNote{TurnID: turn, ItemID: item, Changes: []rpc.FileChange{{Path: "a.go", Kind: "update", Diff: "+x", Additions: 1}}}},
		{"dalgona/advisor/note", rpc.AdvisorNoteNotification{ThreadID: key.Thread, Advisor: "critic", Severity: "warn", Note: "check the loop", Delivery: "inline"}},
		{"dalgona/thread/childSpawned", rpc.ChildSpawnedNotification{ThreadID: key.Thread, ChildID: "child-1", Cwd: "/work"}},
		{"dalgona/sandbox/chosen", rpc.SandboxChosenNotification{ThreadID: key.Thread, Cause: "missing", Choice: "danger"}},
		{"warning", warningNote{Message: "invalid text: answer: too long", RequestID: th.pending[0], Reason: "too long", Field: "answer"}},
		{"warning", warningNote{Message: "disk almost full"}},
	}
	for _, e := range events {
		w.emit(d, key.Thread, e.method, e.params)
	}
	w.settle()
	w.verify()
	tv, _ := w.m.view().Thread(key)
	expectTurnAnnotations(t, tv.Turns[0], plan)
	expectThreadAnnotations(t, tv, Child{Thread: ThreadKey{Endpoint: key.Endpoint, Thread: "child-1"}, Cwd: "/work"})
}

func expectTurnAnnotations(t *testing.T, got TurnView, plan []rpc.TurnPlanStep) {
	t.Helper()
	if !slices.Equal(got.Plan, plan) || got.Effort != "high" || len(got.Items[0].Changes) != 1 || got.Items[0].Changes[0].Path != "a.go" {
		t.Fatalf("turn annotations %+v", got)
	}
}

func expectThreadAnnotations(t *testing.T, tv *ThreadView, child Child) {
	t.Helper()
	if len(tv.Notes) != 1 || tv.Notes[0].Note != "check the loop" || !slices.Equal(tv.Children, []Child{child}) || tv.Sandbox != "danger" || tv.Warning != "disk almost full" {
		t.Fatalf("thread annotations notes=%+v children=%+v sandbox=%q warning=%q", tv.Notes, tv.Children, tv.Sandbox, tv.Warning)
	}
	if warning := tv.Requests[0].Warning; warning == nil || warning.Field != "answer" || warning.Reason != "too long" {
		t.Fatalf("the answer warning is %+v", warning)
	}
}

func TestContentPagesAndReviewsFollowTheItemHeader(t *testing.T) {
	w, d, key := openedSubscribed(t, 23)
	w.settle()
	th := d.threads[key.Thread]
	w.startTurn(d, th)
	w.startItem(d, th)
	w.deltas(d, th, "가나다 body")
	turn, item := th.active.id, th.active.open.id
	w.completeItem(d, th)
	w.emit(d, key.Thread, "dalgona/tool/judged", rpc.ToolJudgedNotification{ThreadID: key.Thread, TurnID: turn, ItemID: item, Verdict: "pass", Summary: "fine"})
	w.settle()
	tv, _ := w.m.view().Thread(key)
	got := tv.Turns[0].Items[0]
	page := ContentKey{Item: got.Key, ContentID: got.Header.Contents[0].ContentID}
	w.handle(ReadContent{Thread: key, Item: page.Item, ContentID: page.ContentID})
	w.handle(ReadContent{Thread: key, Item: page.Item, ContentID: "foreign"})
	w.settle()
	tv, _ = w.m.view().Thread(key)
	loaded, ok := tv.Content(page)
	if !ok || string(loaded.Data) != "가나다 body" || loaded.Next != loaded.Total || loaded.Err != nil {
		t.Fatalf("content page %+v (%v)", loaded, ok)
	}
	if foreign, _ := tv.Content(ContentKey{Item: page.Item, ContentID: "foreign"}); !errors.Is(foreign.Err, errUnknownContent) {
		t.Fatalf("a content id outside the header read %+v", foreign)
	}
	if review := tv.Turns[0].Items[0].Review; review == nil || review.Verdict != "pass" {
		t.Fatalf("item review %+v", review)
	}
	checkExactPageRelease(t, w, key, page)
}

func checkExactPageRelease(t *testing.T, w *modelWorld, key ThreadKey, page ContentKey) {
	t.Helper()
	outstanding := ContentKey{Item: page.Item, ContentID: page.ContentID, Offset: 3}
	w.handle(ReadContent{Thread: key, Item: outstanding.Item, ContentID: outstanding.ContentID, Offset: outstanding.Offset})
	w.handle(ReleaseContentPage{Thread: key, Key: page})
	tv, _ := w.m.view().Thread(key)
	if _, exists := tv.Content(page); exists {
		t.Fatal("the released page is still present")
	}
	if pending, exists := tv.Content(outstanding); !exists || !pending.Loading {
		t.Fatalf("releasing a page dropped a different in-flight read: %+v", pending)
	}
	w.settle()
	tv, _ = w.m.view().Thread(key)
	if other, exists := tv.Content(outstanding); !exists || string(other.Data) != "나다 body" {
		t.Fatalf("the other read did not complete: %+v", other)
	}
}
