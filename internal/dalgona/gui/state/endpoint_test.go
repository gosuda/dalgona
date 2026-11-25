package state

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/synctest"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func row(id string, updated int64) rpc.Thread {
	return rpc.Thread{ID: id, UpdatedAt: updated, Turns: []rpc.Turn{}}
}

func rowIDs(rows []rpc.Thread) []string {
	ids := make([]string, 0, len(rows))
	for _, r := range rows {
		ids = append(ids, r.ID+"/"+r.Name)
	}
	return ids
}

func listCursors(p *fakePort) []string {
	var out []string
	for _, call := range p.calls(methodThreadList) {
		params := call.params.(rpc.ThreadListParams)
		if params.Limit != lanePageLimit {
			return []string{"a limit other than 100"}
		}
		out = append(out, params.Cursor)
	}
	return out
}

type laneServer struct {
	firsts, seconds atomic.Int32
}

func (s *laneServer) list(_ context.Context, call portCall) (any, error) {
	params := call.params.(rpc.ThreadListParams)
	switch {
	case params.Cursor == "c2" && s.seconds.Add(1) == 1:
		return nil, &rpc.RPCError{Code: -32603, Message: "the list is busy"}
	case params.Cursor == "c2":
		return rpc.ThreadListResponse{Data: []rpc.Thread{row("b", 20), row("c", 10)}}, nil
	case s.firsts.Add(1) == 1:
		return rpc.ThreadListResponse{Data: []rpc.Thread{row("a", 30), row("b", 20)}, NextCursor: "c2"}, nil
	}
	return rpc.ThreadListResponse{Data: []rpc.Thread{row("d", 40), row("a", 30)}, NextCursor: "c2"}, nil
}

func expectLane(t *testing.T, lane LaneView, ids []string, failed bool) {
	t.Helper()
	if !slices.Equal(rowIDs(lane.Rows), ids) || (lane.Err != nil) != failed || lane.Loading || !lane.Loaded {
		t.Fatalf("lane rows %q err %v loading %v loaded %v, want %q failed=%v", rowIDs(lane.Rows), lane.Err, lane.Loading, lane.Loaded, ids, failed)
	}
}

func TestLanePagesRetryTheFailedCursorAndReconcileOverlap(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		server := &laneServer{}
		p.handle(methodThreadList, server.list)
		p.owner.Submit(LoadThreads{Endpoint: "local"})
		expectLane(t, settled(p).Lane("local"), []string{"a/", "b/"}, false)
		p.owner.Submit(LoadThreads{Endpoint: "local"})
		expectLane(t, settled(p).Lane("local"), []string{"a/", "b/"}, true)
		p.owner.Submit(LoadThreads{Endpoint: "local"})
		lane := settled(p).Lane("local")
		expectLane(t, lane, []string{"d/", "a/", "b/", "c/"}, false)
		if got := listCursors(p); !slices.Equal(got, []string{"", "c2", "c2", ""}) || lane.More {
			t.Fatalf("thread/list cursors %q, more %v", got, lane.More)
		}
		link := p.link("local")
		p.event(link, "c", "thread/name/updated", rpc.ThreadNameUpdatedNotification{ThreadID: "c", ThreadName: "renamed"})
		p.event(link, "a", "thread/archived", rpc.ThreadArchivedNotification{ThreadID: "a"})
		expectLane(t, settled(p).Lane("local"), []string{"d/", "b/", "c/renamed"}, false)
		p.event(link, "a", "thread/unarchived", rpc.ThreadUnarchivedNotification{ThreadID: "a"})
		expectLane(t, settled(p).Lane("local"), []string{"d/", "a/", "b/", "c/renamed"}, false)
		if summary, ok := settled(p).Summary(ThreadKey{Endpoint: "local", Thread: "c"}); !ok || summary.Name != "renamed" {
			t.Fatalf("summary of c is %+v, %v", summary, ok)
		}
	})
}

func statusBandHandlers(p *fakePort) {
	p.handle(methodModelList, func(context.Context, portCall) (any, error) {
		return rpc.ModelListResponse{Data: []rpc.Model{{ID: "m1", Model: "m1", InputModalities: []string{"text", "image"}}, {ID: "m2", Model: "m2", InputModalities: []string{"text"}}}}, nil
	})
	p.handle(methodConfigRead, func(context.Context, portCall) (any, error) {
		return rpc.ConfigReadResponse{Config: rpc.ConfigValues{Model: "m2", ApprovalPolicy: "on-request", SandboxMode: "workspace-write", ModelReasoningEffort: "low"}}, nil
	})
	p.handle(methodSettingsRead, func(context.Context, portCall) (any, error) {
		return jsontext.Value(`{"settings":[{"key":"approval","value":"ask","defaultValue":"auto","options":["auto","yolo","ask"]},{"key":"verbosity","value":"high","defaultValue":"medium","options":["low","medium","high"]},{"key":"statusHide","value":["cost"],"defaultValue":[],"options":["cost","context"]}]}`), nil
	})
	p.handle(methodThreadStart, func(context.Context, portCall) (any, error) {
		return rpc.ThreadStartResponse{Thread: row("t-new", 99), Model: "m1", ApprovalPolicy: "never", ReasoningEffort: "high"}, nil
	})
	p.handle(methodTurnStart, turnStarted)
}

func oneReceipted(calls []portCall) bool { return len(calls) == 1 && calls[0].op.OperationID != "" }

func expectStartedThread(t *testing.T, p *fakePort, v *View, key ThreadKey) {
	t.Helper()
	starts, turns := p.calls(methodThreadStart), p.calls(methodTurnStart)
	if !oneReceipted(starts) || !oneReceipted(turns) {
		t.Fatalf("new thread sent thread/start %+v and turn/start %+v, want one each with receipts", starts, turns)
	}
	if params := turns[0].params.(rpc.TurnStartParams); params.ThreadID != key.Thread || params.Input[0].Text != "hello there" {
		t.Fatalf("turn/start params %+v", params)
	}
	if started, ok := v.Started("w1"); !ok || started != key || !sameWindows(v.Windows(), []WindowView{{ID: "w1", Threads: []ThreadKey{key}}}) {
		t.Fatalf("started %v (%v) with windows %+v", started, ok, v.Windows())
	}
}

func TestNewThreadStartsThenSendsAndTheStatusBandReadsItsFacts(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.caps.ReceiptMethods = append(p.caps.ReceiptMethods, methodThreadStart)
		statusBandHandlers(p)
		p.owner.Submit(NewThread{Window: "w1", Endpoint: "local", Draft: Draft{Text: "hello there", Revision: 1}})
		v := settled(p)
		key := ThreadKey{Endpoint: "local", Thread: "t-new"}
		expectStartedThread(t, p, v, key)
		ep := v.Endpoints()[slices.IndexFunc(v.Endpoints(), func(e EndpointView) bool { return e.ID == "local" })]
		type band struct{ models, sandbox, approval, verbosity, hide, label string }
		got := band{fmt.Sprint(len(ep.Models)), ep.Config.SandboxMode, ep.Approval, ep.Verbosity, strings.Join(ep.StatusHide, ","), ep.Label}
		if want := (band{"2", "workspace-write", "ask", "high", "cost", "local"}); got != want {
			t.Fatalf("endpoint status band %+v, want %+v", got, want)
		}
		tv, _ := v.Thread(key)
		accepts, known := v.AcceptsImages(key)
		if facts := [5]any{tv.Model, tv.Approval, tv.Effort, accepts, known}; facts != [5]any{"m1", "never", "high", true, true} {
			t.Fatalf("thread facts model, approval, effort, images, known: %v", facts)
		}
	})
}

func TestPanelCallsConnectOnFirstUseAndKeepTheirOutcomeInTheSlot(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.caps.ReceiptMethods = append(p.caps.ReceiptMethods, "thread/fork")
		p.handle("dalgona/thread/tree", func(context.Context, portCall) (any, error) {
			return rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-1"}}}, nil
		})
		p.handle("thread/fork", func(context.Context, portCall) (any, error) {
			return nil, &rpc.RPCError{Code: -32602, Message: "unknown thread"}
		})
		p.owner.Submit(Call{ID: "tree", Endpoint: "local", Method: "dalgona/thread/tree", Params: map[string]string{"threadId": "t1"}})
		p.owner.Submit(Call{ID: "fork", Endpoint: "local", Method: "thread/fork", Params: map[string]string{"threadId": "t1"}, Operation: true})
		v := settled(p)
		tree, _ := v.Call("tree")
		fork, _ := v.Call("fork")
		var rpcErr *rpc.RPCError
		outcomes := [4]any{tree.State, tree.Generation, p.tally(p.connects, "local"), fork.State}
		if outcomes != [4]any{CallSucceeded, client.Generation(1), 1, CallFailed} || !errors.As(fork.Err, &rpcErr) {
			t.Fatalf("tree %+v fork %+v after %d connects", tree, fork, p.tally(p.connects, "local"))
		}
		if forks := p.calls("thread/fork"); !oneReceipted(forks) || len(v.Notices()) != 0 {
			t.Fatalf("fork calls %+v notices %+v", forks, v.Notices())
		}
		p.owner.Submit(CancelCall{ID: "tree"})
		if _, ok := settled(p).Call("tree"); ok {
			t.Fatal("a cancelled call kept its slot")
		}
	})
}

func TestOfflineEndpointFailsAPanelCallAtOnce(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		runOwner(t, t.TempDir(), func(t *testing.T, p *fakePort) {
			p.owner.PhaseChanged(client.PhaseChange{Link: client.Link{Endpoint: "box"}, Phase: client.PhaseOffline})
			p.owner.Submit(Call{ID: "list", Endpoint: "box", Method: "model/list"})
			if c, _ := settled(p).Call("list"); c.State != CallFailed || c.Err == nil || len(p.calls("model/list")) != 0 {
				t.Fatalf("an offline call ended as %+v", c)
			}
		})
	})
}
