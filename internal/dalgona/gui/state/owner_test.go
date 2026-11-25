package state

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"fmt"
	"slices"
	"sync"
	"testing"
	"testing/synctest"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
)

type portCall struct {
	link   client.Link
	method string
	params any
	op     rpc.Operation
}

type answer struct {
	result any
	err    error
}

type fakePort struct {
	t           *testing.T
	mu          sync.Mutex
	owner       *Owner
	caps        rpc.Capabilities
	gens        map[client.EndpointID]client.Generation
	ordinals    map[client.Link]uint64
	scopes      map[client.Link]context.CancelFunc
	contexts    map[client.Link]context.Context
	details     map[string][]byte
	log         []portCall
	handlers    map[string]func(ctx context.Context, call portCall) (any, error)
	connects    map[client.EndpointID]int
	disconnects map[client.EndpointID]int
	attached    map[client.EndpointID]int
}

func newFakePort(t *testing.T) *fakePort {
	return &fakePort{
		t: t, gens: map[client.EndpointID]client.Generation{}, ordinals: map[client.Link]uint64{}, scopes: map[client.Link]context.CancelFunc{},
		contexts: map[client.Link]context.Context{}, details: map[string][]byte{},
		handlers: map[string]func(context.Context, portCall) (any, error){}, connects: map[client.EndpointID]int{},
		disconnects: map[client.EndpointID]int{}, attached: map[client.EndpointID]int{},
		caps: rpc.Capabilities{ReceiptMethods: []string{methodTurnStart, methodTurnSteer, methodTurnInterrupt}},
	}
}

func streamOf(link client.Link) string { return fmt.Sprintf("%s-%d", link.Endpoint, link.Generation) }

func (p *fakePort) link(id client.EndpointID) client.Link {
	p.mu.Lock()
	defer p.mu.Unlock()
	return client.Link{Endpoint: id, Generation: p.gens[id]}
}

func (p *fakePort) Connect(id client.EndpointID) error {
	p.mu.Lock()
	p.connects[id]++
	p.gens[id]++
	link := client.Link{Endpoint: id, Generation: p.gens[id]}
	ctx, cancel := context.WithCancel(context.Background())
	p.contexts[link], p.scopes[link] = ctx, cancel
	caps := p.caps
	p.mu.Unlock()
	p.owner.PhaseChanged(client.PhaseChange{Link: link, Phase: client.PhaseConnected, Native: rpc.NativeSession{StreamID: streamOf(link)}, Capabilities: caps})
	return nil
}

func (p *fakePort) Disconnect(id client.EndpointID) error {
	p.mu.Lock()
	p.disconnects[id]++
	p.mu.Unlock()
	p.lose(id, client.PhaseIdle)
	return nil
}

func (p *fakePort) lose(id client.EndpointID, phase client.Phase) {
	link := p.link(id)
	p.mu.Lock()
	if cancel, ok := p.scopes[link]; ok {
		cancel()
	}
	p.mu.Unlock()
	p.owner.PhaseChanged(client.PhaseChange{Link: link, Phase: phase})
}

func (p *fakePort) Attach(id client.EndpointID) (func(), error) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.attached[id]++
	return func() {
		p.mu.Lock()
		defer p.mu.Unlock()
		p.attached[id]--
	}, nil
}

func (p *fakePort) Call(ctx context.Context, link client.Link, method string, params, result any) error {
	return p.dispatch(ctx, portCall{link: link, method: method, params: params}, result)
}

func (p *fakePort) CallOperation(ctx context.Context, link client.Link, op rpc.Operation, params, result any) error {
	return p.dispatch(ctx, portCall{link: link, method: op.Method, params: params, op: op}, result)
}

const methodRespond = "test/respond"

type correction struct {
	request string
	result  any
}

func (p *fakePort) Respond(ctx context.Context, link client.Link, requestID string, result any) error {
	p.mu.Lock()
	p.log = append(p.log, portCall{link: link, method: methodRespond, params: correction{request: requestID, result: result}})
	handler, ok := p.handlers[methodRespond]
	p.mu.Unlock()
	if !ok {
		return nil
	}
	_, err := handler(ctx, portCall{link: link, method: methodRespond, params: correction{request: requestID, result: result}})
	return err
}

func (p *fakePort) Endpoints() []client.EndpointStatus {
	return []client.EndpointStatus{{ID: "box", Label: "box", Kind: client.EndpointBox}, {ID: "local", Label: "local", Kind: client.EndpointLocal}}
}

func (p *fakePort) dispatch(ctx context.Context, call portCall, result any) error {
	p.mu.Lock()
	p.log = append(p.log, call)
	handler, ok := p.handlers[call.method]
	p.mu.Unlock()
	if !ok {
		handler = p.standard
	}
	value, err := handler(ctx, call)
	if err != nil {
		return err
	}
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	return json.Unmarshal(raw, result)
}

func (p *fakePort) standard(_ context.Context, call portCall) (any, error) {
	switch params := call.params.(type) {
	case openParams:
		p.mu.Lock()
		cut := p.ordinals[call.link]
		p.mu.Unlock()
		return rpc.HistorySnapshot{
			HistoryID: fmt.Sprintf("h-%s-%s-%d", params.ThreadID, streamOf(call.link), cut), Thread: rpc.Thread{ID: params.ThreadID},
			PathRevision: "path", StreamID: streamOf(call.link), CutOrdinal: rpc.FormatDecimal(cut), PendingRequests: []rpc.PendingRequest{},
		}, nil
	case pageParams:
		return rpc.HistoryPage{Turns: []rpc.HistoryTurn{}, PathRevision: "path"}, nil
	case detailsParams:
		p.mu.Lock()
		data := p.details[params.RequestID]
		p.mu.Unlock()
		sum := sha256.Sum256(data)
		total := rpc.FormatDecimal(uint64(len(data)))
		return rpc.ContentChunk{DataBase64: base64.StdEncoding.EncodeToString(data), Offset: "0", Next: total, Total: total, Digest: hex.EncodeToString(sum[:])}, nil
	case historyParams, threadParams:
		return map[string]string{}, nil
	}
	return nil, rpc.NativeKind(rpc.ErrorNotFound)
}

func (p *fakePort) handle(method string, handler func(ctx context.Context, call portCall) (any, error)) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.handlers[method] = handler
}

func (p *fakePort) tally(counts map[client.EndpointID]int, id client.EndpointID) int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return counts[id]
}

func (p *fakePort) calls(method string) []portCall {
	p.mu.Lock()
	defer p.mu.Unlock()
	var out []portCall
	for _, c := range p.log {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

func (p *fakePort) next(link client.Link) uint64 {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.ordinals[link]++
	return p.ordinals[link]
}

func (p *fakePort) event(link client.Link, thread, method string, params any) {
	p.deliverEvent(link, p.next(link), thread, method, params)
}

func (p *fakePort) deliverEvent(link client.Link, ordinal uint64, thread, method string, params any) {
	event := rpc.EventParams{
		StreamID: streamOf(link), FirstOrdinal: rpc.FormatDecimal(ordinal), LastOrdinal: rpc.FormatDecimal(ordinal),
		ThreadID: thread, Method: method, Params: mustJSON(p.t, params),
	}
	p.owner.Notified(link, rpc.MethodEvent, mustJSON(p.t, event))
}

func requestHeader(p *fakePort, link client.Link, ordinal uint64, thread, id string) jsontext.Value {
	data := mustJSON(p.t, map[string]string{"threadId": thread, "command": "make test"})
	p.mu.Lock()
	p.details[id] = data
	p.mu.Unlock()
	sum := sha256.Sum256(data)
	return mustJSON(p.t, rpc.RequestParams{
		Method:       "item/commandExecution/requestApproval",
		Details:      rpc.RequestDetails{RequestID: id, Total: rpc.FormatDecimal(uint64(len(data))), Digest: hex.EncodeToString(sum[:])},
		DalgonaEvent: rpc.EventPosition{StreamID: streamOf(link), Ordinal: rpc.FormatDecimal(ordinal), ThreadID: thread},
	})
}

func (p *fakePort) request(link client.Link, ordinal uint64, thread, id string) <-chan answer {
	params := requestHeader(p, link, ordinal, thread, id)
	p.mu.Lock()
	ctx := p.contexts[link]
	p.mu.Unlock()
	out := make(chan answer, 1)
	go func() {
		result, err := p.owner.Requested(ctx, link, rpc.MethodRequest, params)
		out <- answer{result: result, err: err}
	}()
	synctest.Wait()
	return out
}

func withOwner(t *testing.T, body func(t *testing.T, p *fakePort)) {
	synctest.Test(t, func(t *testing.T) { runOwner(t, t.TempDir(), body) })
}

func runOwner(t *testing.T, dir string, body func(t *testing.T, p *fakePort)) {
	p := newFakePort(t)
	p.owner = New(func() {}, dir)
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { done <- p.owner.Run(ctx, p) }()
	body(t, p)
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	p.mu.Lock()
	for _, stop := range p.scopes {
		stop()
	}
	p.mu.Unlock()
}

func settled(p *fakePort) *View {
	synctest.Wait()
	return p.owner.View()
}

func threadView(t *testing.T, p *fakePort, key ThreadKey) *ThreadView {
	t.Helper()
	tv, ok := settled(p).Thread(key)
	if !ok {
		t.Fatalf("no view for %v", key)
	}
	return tv
}

var (
	localThread = ThreadKey{Endpoint: "local", Thread: "t1"}
	localDraft  = DraftKey{Endpoint: "local", Window: "w1", Thread: "t1"}
)

func startStreaming(p *fakePort, link client.Link, thread string, item ItemKey, text string) {
	p.event(link, thread, "turn/started", turnNote{Turn: liveTurn{ID: item.Turn, Status: turnInProgress}})
	p.event(link, thread, "item/started", itemNote{TurnID: item.Turn, Item: rpc.ItemHeader{ID: item.Item, Type: rpc.KindAgent, Metadata: map[string]jsontext.Value{}, Contents: []rpc.ContentReference{}}})
	p.event(link, thread, "item/agentMessage/delta", deltaNote{TurnID: item.Turn, ItemID: item.Item, Delta: text})
}

func openStreaming(p *fakePort, key ThreadKey, text string) {
	p.owner.Submit(OpenView{Window: "w1", Thread: key})
	synctest.Wait()
	startStreaming(p, p.link(key.Endpoint), key.Thread, ItemKey{Turn: "turn-1", Item: "item-1"}, text)
}

func tailOf(t *testing.T, tv *ThreadView) string {
	t.Helper()
	if len(tv.Turns) != 1 || len(tv.Turns[0].Items) != 1 {
		t.Fatalf("view %v has turns %+v, want one turn with one item", tv.Key, tv.Turns)
	}
	return string(tv.Turns[0].Items[0].Tail)
}

func expectTail(t *testing.T, tv *ThreadView, want string) {
	t.Helper()
	if got := tailOf(t, tv); got != want {
		t.Fatalf("%v reads %q, want %q", tv.Key, got, want)
	}
}

func expectFrozen(t *testing.T, before, after *ThreadView) {
	t.Helper()
	if tailOf(t, after) != tailOf(t, before) || after.Live || !after.Offline || after.Applied != before.Applied {
		t.Fatalf("the offline view changed from %+v to %+v", before, after)
	}
}

func expectOfflineSendRefused(t *testing.T, p *fakePort) {
	t.Helper()
	p.owner.Submit(Send{Key: localDraft, Draft: Draft{Text: "are you there", Revision: 3}})
	v := settled(p)
	if len(p.calls(methodTurnStart)) != 0 || v.Draft(localDraft).Text != "are you there" || len(v.Notices()) != 1 {
		t.Fatalf("an offline send dispatched %d turns, kept draft %q, and showed %v", len(p.calls(methodTurnStart)), v.Draft(localDraft).Text, v.Notices())
	}
}

func TestOfflineEndpointFreezesWhileTheOtherKeepsApplying(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		box := ThreadKey{Endpoint: "box", Thread: localThread.Thread}
		openStreaming(p, localThread, "hello")
		openStreaming(p, box, "other")
		expectTail(t, threadView(t, p, localThread), "hello")
		lost := p.link("local")
		p.lose("local", client.PhaseOffline)
		frozen := threadView(t, p, localThread)
		p.deliverEvent(lost, p.next(lost), "t1", "item/agentMessage/delta", deltaNote{TurnID: "turn-1", ItemID: "item-1", Delta: " late"})
		p.event(p.link("box"), "t1", "item/agentMessage/delta", deltaNote{TurnID: "turn-1", ItemID: "item-1", Delta: " more"})
		expectFrozen(t, frozen, threadView(t, p, localThread))
		expectTail(t, threadView(t, p, box), "other more")
		expectOfflineSendRefused(t, p)
		p.owner.Submit(Connect{Endpoint: "local"})
		if back := threadView(t, p, localThread); !back.Live || back.Offline {
			t.Fatalf("after an explicit connect local is live=%v offline=%v", back.Live, back.Offline)
		}
	})
}

func expectMutation(t *testing.T, v *View, state MutationState) MutationView {
	t.Helper()
	ms := v.Mutations()
	if len(ms) != 1 || ms[0].State != state {
		t.Fatalf("mutations %+v, want one in state %d", ms, state)
	}
	return ms[0]
}

func turnStarted(context.Context, portCall) (any, error) {
	return map[string]any{"turn": map[string]string{"id": "turn-9", "status": turnInProgress}}, nil
}

func sendAcrossALoss(t *testing.T, p *fakePort, receipt func(context.Context, portCall) (any, error)) *View {
	p.handle(methodTurnStart, func(ctx context.Context, call portCall) (any, error) {
		if call.link.Generation > 1 {
			return turnStarted(ctx, call)
		}
		<-ctx.Done()
		return nil, &rpc.UnknownOutcomeError{Method: call.op.Method, OperationID: call.op.OperationID, Cause: ctx.Err()}
	})
	p.handle(methodOperationRead, receipt)
	p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
	synctest.Wait()
	p.owner.Submit(Send{Key: localDraft, Draft: Draft{Text: "ship it", Revision: 1}})
	p.owner.Submit(PutDraft{Key: localDraft, Draft: Draft{Text: "next idea", Revision: 2}})
	expectMutation(t, settled(p), MutationDispatched)
	p.lose("local", client.PhaseReconnecting)
	v := settled(p)
	if mu := expectMutation(t, v, MutationUnknown); mu.Payload.Text != "ship it" || v.Draft(localDraft).Text != "next idea" {
		t.Fatalf("after the loss the payload is %q and the draft %q", mu.Payload.Text, v.Draft(localDraft).Text)
	}
	if err := p.Connect("local"); err != nil {
		t.Fatal(err)
	}
	v = settled(p)
	starts, reads := p.calls(methodTurnStart), p.calls(methodOperationRead)
	if len(starts) != 1 || len(reads) != 1 || reads[0].params.(operationParams).OperationID != starts[0].op.OperationID {
		t.Fatalf("reconnect sent %d turn/start and %d receipt reads", len(starts), len(reads))
	}
	if v.Draft(localDraft).Text != "next idea" {
		t.Fatalf("reconciliation changed the draft to %q", v.Draft(localDraft).Text)
	}
	return v
}

func TestUnknownOutcomeIsReconciledThroughItsReceiptNeverResent(t *testing.T) {
	t.Run("succeeded", func(t *testing.T) {
		withOwner(t, func(t *testing.T, p *fakePort) {
			v := sendAcrossALoss(t, p, func(context.Context, portCall) (any, error) {
				return rpc.OperationRecord{State: "succeeded", Method: methodTurnStart, Result: jsontext.Value(`{"turn":{"id":"turn-9","status":"inProgress"}}`)}, nil
			})
			if len(v.Mutations()) != 0 {
				t.Fatalf("a succeeded receipt left %+v", v.Mutations())
			}
		})
	})
	t.Run("never started", func(t *testing.T) {
		withOwner(t, func(t *testing.T, p *fakePort) {
			v := sendAcrossALoss(t, p, func(context.Context, portCall) (any, error) { return nil, rpc.NativeKind(rpc.ErrorNotFound) })
			p.owner.Submit(Retry{OperationID: expectMutation(t, v, MutationFailed).OperationID})
			v = settled(p)
			starts := p.calls(methodTurnStart)
			if len(starts) != 2 || starts[1].op.OperationID == starts[0].op.OperationID || len(v.Mutations()) != 0 {
				t.Fatalf("an explicit retry sent %d turn/start (%+v) leaving %+v", len(starts), starts, v.Mutations())
			}
		})
	})
}

func expectApplied(t *testing.T, tv *ThreadView, applied uint64, name string) {
	t.Helper()
	if tv.Applied != applied || tv.Thread.Name != name {
		t.Fatalf("applied %d with name %q, want %d with %q", tv.Applied, tv.Thread.Name, applied, name)
	}
}

func TestRequestsTakeTheirStreamPlaceAndAMissingOrdinalResyncsAtOnce(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
		synctest.Wait()
		link := p.link("local")
		p.event(link, "t1", "turn/started", turnNote{Turn: liveTurn{ID: "turn-1", Status: turnInProgress}})
		p.request(link, p.next(link), "t1", "req-1")
		p.event(link, "t1", "thread/name/updated", rpc.ThreadNameUpdatedNotification{ThreadID: "t1", ThreadName: "after the request"})
		tv := threadView(t, p, localThread)
		expectApplied(t, tv, 3, "after the request")
		if len(tv.Requests) != 1 || tv.Requests[0].Arrival == 0 || p.tally(p.disconnects, "local") != 0 {
			t.Fatalf("an in-order request left requests %+v and %d resyncs", tv.Requests, p.tally(p.disconnects, "local"))
		}
		p.next(link)
		p.event(link, "t1", "thread/name/updated", rpc.ThreadNameUpdatedNotification{ThreadID: "t1", ThreadName: "never"})
		tv = threadView(t, p, localThread)
		disconnects, connects := p.tally(p.disconnects, "local"), p.tally(p.connects, "local")
		if disconnects != 1 || connects != 2 || tv.Thread.Name == "never" || !tv.Live {
			t.Fatalf("after a skipped ordinal: disconnects %d, connects %d, name %q, live %v", disconnects, connects, tv.Thread.Name, tv.Live)
		}
	})
}

func expectAnswer(t *testing.T, replies <-chan answer, want string) {
	t.Helper()
	got := <-replies
	if raw, ok := got.result.(jsontext.Value); got.err != nil || !ok || string(raw) != want {
		t.Fatalf("the request was answered with %v, %v; want %s", got.result, got.err, want)
	}
}

func expectUnanswered(t *testing.T, replies <-chan answer) {
	t.Helper()
	select {
	case got := <-replies:
		t.Fatalf("the request was answered with %v, %v", got.result, got.err)
	default:
	}
}

func expectReleased(t *testing.T, p *fakePort, v *View) {
	t.Helper()
	attached := p.tally(p.attached, "local")
	if _, open := v.Thread(localThread); open || len(p.calls(methodUnsubscribe)) != 1 || len(p.calls(methodClose)) != 1 || attached != 0 {
		t.Fatalf("the last close left open=%v, unsubscribed %d, closed %d, attached %d", open, len(p.calls(methodUnsubscribe)), len(p.calls(methodClose)), attached)
	}
}

func TestClosingViewsFollowsLastSubscriberRules(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
		p.owner.Submit(OpenView{Window: "w2", Thread: localThread})
		synctest.Wait()
		link := p.link("local")
		answered := p.request(link, p.next(link), "t1", "req-answered")
		pending := p.request(link, p.next(link), "t1", "req-pending")
		if v := settled(p); len(p.calls(methodOpen)) != 1 || len(v.PendingDecisions()) != 2 {
			t.Fatalf("two windows opened %d snapshots and list %d decisions", len(p.calls(methodOpen)), len(v.PendingDecisions()))
		}
		p.owner.Submit(Answer{Request: RequestKey{Thread: localThread, Request: "req-answered"}, Result: jsontext.Value(`{"decision":"accept"}`)})
		p.owner.Submit(Answer{Request: RequestKey{Thread: localThread, Request: "req-answered"}, Result: jsontext.Value(`{"decision":"decline"}`)})
		synctest.Wait()
		expectAnswer(t, answered, `{"decision":"accept"}`)
		p.owner.Submit(CloseView{Window: "w1", Thread: localThread})
		tv := threadView(t, p, localThread)
		if len(p.calls(methodUnsubscribe)) != 0 || len(tv.Requests) != 2 || !tv.Requests[1].Actionable || !tv.Requests[0].Answered {
			t.Fatalf("closing one of two views unsubscribed %d times and left %+v", len(p.calls(methodUnsubscribe)), tv.Requests)
		}
		p.owner.Submit(CloseView{Window: "w2", Thread: localThread})
		expectReleased(t, p, settled(p))
		expectUnanswered(t, pending)
	})
}

func expectStarts(t *testing.T, p *fakePort, queued int, texts ...string) {
	t.Helper()
	left := len(settled(p).FollowUps(localDraft))
	starts := p.calls(methodTurnStart)
	got := make([]string, 0, len(starts))
	for _, call := range starts {
		got = append(got, call.params.(rpc.TurnStartParams).Input[0].Text)
	}
	if !slices.Equal(got, texts) || left != queued {
		t.Fatalf("sent %q with %d queued, want %q with %d", got, left, texts, queued)
	}
}

func TestFollowUpsSendOneAtATimeAfterEachTurn(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.handle(methodTurnStart, turnStarted)
		p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
		synctest.Wait()
		link := p.link("local")
		p.event(link, "t1", "turn/started", turnNote{Turn: liveTurn{ID: "turn-1", Status: turnInProgress}})
		p.owner.Submit(Enqueue{Key: localDraft, Draft: Draft{Text: "first", Revision: 1}})
		p.owner.Submit(Enqueue{Key: localDraft, Draft: Draft{Text: "second", Revision: 2}})
		expectStarts(t, p, 2)
		p.event(link, "t1", "turn/completed", turnNote{Turn: liveTurn{ID: "turn-1", Status: "completed"}})
		expectStarts(t, p, 1, "first")
		p.event(link, "t1", "turn/started", turnNote{Turn: liveTurn{ID: "turn-9", Status: turnInProgress}})
		p.event(link, "t1", "turn/completed", turnNote{Turn: liveTurn{ID: "turn-9", Status: "completed"}})
		expectStarts(t, p, 0, "first", "second")
	})
}
