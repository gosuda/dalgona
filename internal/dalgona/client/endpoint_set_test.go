package client

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type fakeDaemon struct {
	withoutNative bool
	capabilities  rpc.Capabilities
	early         string
	hold          chan struct{}
	mu            sync.Mutex
	conns         []*rpc.Conn
	echoes        int
}

func newFakeDaemon() *fakeDaemon {
	return &fakeDaemon{
		hold: make(chan struct{}),
		capabilities: rpc.Capabilities{
			Version: 1, DaemonInstanceID: "daemon", TerminalCheckpointVersion: 1,
			Features: map[string]int{
				"orderedEvents": 1, "requestCancellation": 1, "history": 1, "mutationReceipts": 1,
				"imageRead": 1, "threadUnsubscribe": 1, "auth": 1, "mcpConfig": 1, "workspace": 1,
				"terminal": 1, "runnerManagement": 1, "diagnostics": 1, "failureRecords": 1, "reporting": 1,
			},
		},
	}
}

func (d *fakeDaemon) Dial(ctx context.Context) (io.ReadWriteCloser, error) { return d.dialPipe(ctx) }

func (d *fakeDaemon) dialPipe(context.Context) (net.Conn, error) {
	near, far := net.Pipe()
	var conn *rpc.Conn
	conn = rpc.NewConn(context.Background(), far, func(ctx context.Context, method string, params jsontext.Value) (jsontext.Value, error) {
		return d.handle(ctx, conn, method, params)
	})
	d.mu.Lock()
	d.conns = append(d.conns, conn)
	d.mu.Unlock()
	return near, nil
}

func (d *fakeDaemon) handle(ctx context.Context, conn *rpc.Conn, method string, params jsontext.Value) (jsontext.Value, error) {
	switch method {
	case "initialize":
		if d.early != "" {
			_ = conn.Notify(d.early, nil)
		}
		if d.withoutNative {
			return json.Marshal(rpc.InitializeResult{UserAgent: "old daemon"})
		}
		return json.Marshal(rpc.InitializeResult{DalgonaNative: &rpc.NativeSession{Version: 1, Delivery: "ordered-v1", DaemonInstanceID: "daemon", StreamID: "stream"}})
	case "dalgona/capabilities/read":
		return json.Marshal(d.capabilities)
	case "test/echo":
		d.mu.Lock()
		d.echoes++
		d.mu.Unlock()
		return params, nil
	case "test/hold":
		select {
		case <-d.hold:
		case <-ctx.Done():
		}
		return jsontext.Value("null"), nil
	}
	return nil, &rpc.RPCError{Code: -32601, Message: "Method not found"}
}

func (d *fakeDaemon) notify(t *testing.T, method string) {
	t.Helper()
	d.mu.Lock()
	conn := d.conns[len(d.conns)-1]
	d.mu.Unlock()
	if err := conn.Notify(method, nil); err != nil {
		t.Fatalf("notify %s: %v", method, err)
	}
}

func (d *fakeDaemon) kill() {
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, conn := range d.conns {
		_ = conn.Close()
	}
}

func (d *fakeDaemon) echoCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.echoes
}

type stamp struct {
	At    time.Duration
	Phase Phase
}

type recorder struct {
	start   time.Time
	gate    chan struct{}
	mu      sync.Mutex
	changes []PhaseChange
	stamps  map[EndpointID][]stamp
	events  []string
}

func newRecorder() *recorder {
	return &recorder{start: time.Now(), stamps: map[EndpointID][]stamp{}}
}

func (r *recorder) PhaseChanged(change PhaseChange) {
	if r.gate != nil {
		<-r.gate
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.changes = append(r.changes, change)
	r.stamps[change.Link.Endpoint] = append(r.stamps[change.Link.Endpoint], stamp{At: time.Since(r.start), Phase: change.Phase})
	r.events = append(r.events, fmt.Sprintf("%s#%d %s", change.Link.Endpoint, change.Link.Generation, change.Phase))
}

func (r *recorder) Notified(link Link, method string, _ jsontext.Value) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, fmt.Sprintf("%s#%d %s", link.Endpoint, link.Generation, method))
}

func (r *recorder) Requested(context.Context, Link, string, jsontext.Value) (any, error) {
	return nil, errors.New("no requests expected")
}

func (r *recorder) stampsOf(id EndpointID) []stamp {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.stamps[id])
}

func (r *recorder) eventLog() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.events)
}

func (r *recorder) last(id EndpointID) PhaseChange {
	r.mu.Lock()
	defer r.mu.Unlock()
	for _, change := range slices.Backward(r.changes) {
		if change.Link.Endpoint == id {
			return change
		}
	}
	return PhaseChange{}
}

func (r *recorder) link(t *testing.T, id EndpointID) Link {
	t.Helper()
	change := r.last(id)
	if change.Phase != PhaseConnected {
		t.Fatalf("%s is %s (%v), want connected", id, change.Phase, change.Err)
	}
	return change.Link
}

type dialClock struct {
	start time.Time
	mu    sync.Mutex
	at    []time.Duration
}

func (c *dialClock) record() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.at = append(c.at, time.Since(c.start))
}

func (c *dialClock) times() []time.Duration {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.at)
}

func failingDial(clock *dialClock, cause error) func(context.Context) (io.ReadWriteCloser, error) {
	return func(context.Context) (io.ReadWriteCloser, error) {
		clock.record()
		return nil, cause
	}
}

func firstDialOnly(clock *dialClock, daemon *fakeDaemon, cause error) func(context.Context) (io.ReadWriteCloser, error) {
	return func(ctx context.Context) (io.ReadWriteCloser, error) {
		clock.record()
		if len(clock.times()) == 1 {
			return daemon.Dial(ctx)
		}
		return nil, cause
	}
}

func openSet(t *testing.T, observer Observer, specs ...EndpointSpec) *EndpointSet {
	t.Helper()
	set, err := NewEndpointSet(NewRemotes(config.Config{Home: t.TempDir()}, nil), specs, observer, ProtocolNative)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = set.Close() })
	return set
}

func requireStamps(t *testing.T, id EndpointID, got, want []stamp) {
	t.Helper()
	if !slices.Equal(got, want) {
		t.Fatalf("%s phases = %v, want %v", id, got, want)
	}
}

func requireDialTimes(t *testing.T, got []time.Duration, want ...time.Duration) {
	t.Helper()
	if !slices.Equal(got, want) {
		t.Fatalf("dials at %v, want %v", got, want)
	}
}

func boxSpecs(dial func(context.Context) (io.ReadWriteCloser, error)) []EndpointSpec {
	specs := make([]EndpointSpec, MaxLiveEndpoints+1)
	for i := range specs {
		specs[i] = EndpointSpec{ID: EndpointID(fmt.Sprintf("box%d", i)), Label: fmt.Sprintf("box%d", i), Kind: EndpointBox, Dial: dial}
	}
	return specs
}

func idsOf(specs []EndpointSpec) []EndpointID {
	ids := make([]EndpointID, len(specs))
	for i, spec := range specs {
		ids[i] = spec.ID
	}
	return ids
}

func connectEach(t *testing.T, set *EndpointSet, ids ...EndpointID) {
	t.Helper()
	for _, id := range ids {
		if err := set.Connect(id); err != nil {
			t.Fatalf("connect %s: %v", id, err)
		}
		synctest.Wait()
	}
}

func attachEach(t *testing.T, set *EndpointSet, ids ...EndpointID) {
	t.Helper()
	for _, id := range ids {
		detach, err := set.Attach(id)
		if err != nil {
			t.Fatalf("attach %s: %v", id, err)
		}
		t.Cleanup(detach)
	}
}

func requirePhases(t *testing.T, rec *recorder, want map[EndpointID]Phase) {
	t.Helper()
	for id, phase := range want {
		if got := rec.last(id); got.Phase != phase {
			t.Fatalf("%s is %s (%v), want %s", id, got.Phase, got.Err, phase)
		}
	}
}

func samePhase(ids []EndpointID, phase Phase) map[EndpointID]Phase {
	phases := make(map[EndpointID]Phase, len(ids))
	for _, id := range ids {
		phases[id] = phase
	}
	return phases
}

func requireNotified(t *testing.T, rec *recorder, events ...string) {
	t.Helper()
	for _, event := range events {
		if !slices.Contains(rec.eventLog(), event) {
			t.Fatalf("notification %q missing from %v", event, rec.eventLog())
		}
	}
}

func TestConnectFailuresFollowTheRetryLadderThenWaitForAnExplicitConnect(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		refused := errors.New("connection refused")
		rec := newRecorder()
		clock := &dialClock{start: time.Now()}
		set := openSet(t, rec, EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: failingDial(clock, refused)})
		if err := set.Connect("work"); err != nil {
			t.Fatal(err)
		}
		time.Sleep(time.Minute)
		synctest.Wait()
		requireDialTimes(t, clock.times(), 0, time.Second, 3*time.Second, 7*time.Second)
		requireStamps(t, "work", rec.stampsOf("work"), []stamp{
			{0, PhaseConnecting},
			{0, PhaseReconnecting},
			{time.Second, PhaseReconnecting},
			{3 * time.Second, PhaseReconnecting},
			{7 * time.Second, PhaseOffline},
		})
		if last := rec.last("work"); !errors.Is(last.Err, refused) {
			t.Fatalf("offline cause = %v, want %v", last.Err, refused)
		}
		if err := set.Connect("work"); err != nil {
			t.Fatal(err)
		}
		time.Sleep(time.Minute)
		synctest.Wait()
		requireDialTimes(t, clock.times(), 0, time.Second, 3*time.Second, 7*time.Second, time.Minute, time.Minute+time.Second, time.Minute+3*time.Second, time.Minute+7*time.Second)
	})
}

func TestLosingOneEndpointFreezesOnlyThatEndpoint(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		const local = EndpointID("/home/alpha/.dalgona")
		rec := newRecorder()
		home, work := newFakeDaemon(), newFakeDaemon()
		defer close(home.hold)
		defer close(work.hold)
		workClock := &dialClock{start: time.Now()}
		set := openSet(t, rec,
			EndpointSpec{ID: local, Label: "local", Kind: EndpointLocal, Dial: home.Dial},
			EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: firstDialOnly(workClock, work, errors.New("connection refused"))},
		)
		connectEach(t, set, local, "work")
		localLink, workLink := rec.link(t, local), rec.link(t, "work")
		home.notify(t, "test/before")
		synctest.Wait()

		lostAt := time.Since(rec.start)
		work.kill()
		synctest.Wait()
		home.notify(t, "test/during")
		synctest.Wait()
		var echoed string
		if err := set.Call(t.Context(), localLink, "test/echo", "still live", &echoed); err != nil || echoed != "still live" {
			t.Fatalf("local call while work reconnects = %q, %v", echoed, err)
		}
		if err := set.Call(t.Context(), workLink, "test/echo", "frozen", nil); !errors.Is(err, ErrStaleLink) {
			t.Fatalf("work call after loss = %v, want ErrStaleLink", err)
		}

		time.Sleep(time.Minute)
		synctest.Wait()
		home.notify(t, "test/after")
		synctest.Wait()
		requireDialTimes(t, workClock.times(), 0, lostAt+time.Second, lostAt+3*time.Second, lostAt+7*time.Second)
		requireStamps(t, "work", rec.stampsOf("work"), []stamp{
			{0, PhaseConnecting},
			{0, PhaseConnected},
			{lostAt, PhaseReconnecting},
			{lostAt + time.Second, PhaseReconnecting},
			{lostAt + 3*time.Second, PhaseReconnecting},
			{lostAt + 7*time.Second, PhaseOffline},
		})
		requireStamps(t, local, rec.stampsOf(local), []stamp{{0, PhaseConnecting}, {0, PhaseConnected}})
		requireNotified(t, rec, string(local)+"#1 test/before", string(local)+"#1 test/during", string(local)+"#1 test/after")
	})
}

func TestNinthConnectEvictsTheLeastRecentIdleEndpoint(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		rec := newRecorder()
		daemon := newFakeDaemon()
		specs := boxSpecs(daemon.Dial)
		set := openSet(t, rec, specs...)
		attachEach(t, set, "box0")
		connectEach(t, set, idsOf(specs[:MaxLiveEndpoints])...)
		held := make(chan error, 1)
		box1 := rec.link(t, "box1")
		go func() { held <- set.Call(context.Background(), box1, "test/hold", nil, nil) }()
		synctest.Wait()

		connectEach(t, set, "box8")
		requirePhases(t, rec, map[EndpointID]Phase{"box0": PhaseConnected, "box1": PhaseConnected, "box2": PhaseIdle, "box3": PhaseConnected, "box8": PhaseConnected})
		close(daemon.hold)
		if err := <-held; err != nil {
			t.Fatalf("held call on box1: %v", err)
		}
	})
}

func TestLiveLimitRefusesAConnectWhenEveryLiveEndpointIsBusy(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		rec := newRecorder()
		daemon := newFakeDaemon()
		defer close(daemon.hold)
		specs := boxSpecs(daemon.Dial)
		set := openSet(t, rec, specs...)
		live := idsOf(specs[:MaxLiveEndpoints])
		attachEach(t, set, live...)
		connectEach(t, set, live...)
		if err := set.Connect("box8"); !errors.Is(err, ErrLiveLimit) {
			t.Fatalf("ninth connect with eight attached endpoints = %v, want ErrLiveLimit", err)
		}
		synctest.Wait()
		requirePhases(t, rec, samePhase(live, PhaseConnected))
		if got := rec.stampsOf("box8"); len(got) != 0 {
			t.Fatalf("refused endpoint changed phase: %v", got)
		}
	})
}

func TestCallsReachOnlyTheCurrentLink(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		rec := newRecorder()
		daemon := newFakeDaemon()
		defer close(daemon.hold)
		set := openSet(t, rec, EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: daemon.Dial})
		if err := set.Connect("work"); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		first := rec.link(t, "work")
		if err := set.Disconnect("work"); err != nil {
			t.Fatal(err)
		}
		if err := set.Connect("work"); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		second := rec.link(t, "work")
		if second.Generation == first.Generation {
			t.Fatalf("reconnect kept generation %d", first.Generation)
		}
		err := set.CallOperation(t.Context(), first, rpc.Operation{Method: "test/echo", OperationID: rpc.NewOperationID()}, map[string]string{}, nil)
		if !errors.Is(err, ErrStaleLink) || daemon.echoCount() != 0 {
			t.Fatalf("operation on the old link = %v with %d dispatches, want ErrStaleLink and none", err, daemon.echoCount())
		}
		if err := set.Call(t.Context(), second, "test/echo", "current", nil); err != nil || daemon.echoCount() != 1 {
			t.Fatalf("call on the current link = %v with %d dispatches", err, daemon.echoCount())
		}
	})
}

func TestConnectedPhaseReachesTheObserverBeforeTheLinkNotifies(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		rec := newRecorder()
		rec.gate = make(chan struct{})
		daemon := newFakeDaemon()
		daemon.early = "test/early"
		defer close(daemon.hold)
		set := openSet(t, rec, EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: daemon.Dial})
		if err := set.Connect("work"); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		if got := rec.eventLog(); len(got) != 0 {
			t.Fatalf("events before the first phase change was delivered: %v", got)
		}
		close(rec.gate)
		synctest.Wait()
		want := []string{"work#0 connecting", "work#1 connected", "work#1 test/early"}
		if got := rec.eventLog(); !slices.Equal(got, want) {
			t.Fatalf("events = %v, want %v", got, want)
		}
	})
}

func TestFailuresThatRetryingCannotFixStopTheLadder(t *testing.T) {
	for _, test := range []struct {
		name  string
		dial  func(*fakeDaemon) func(context.Context) (io.ReadWriteCloser, error)
		phase Phase
	}{
		{"daemon without the native protocol", func(d *fakeDaemon) func(context.Context) (io.ReadWriteCloser, error) {
			d.withoutNative = true
			return d.Dial
		}, PhaseIncompatible},
		{"daemon of another build", func(*fakeDaemon) func(context.Context) (io.ReadWriteCloser, error) {
			return func(context.Context) (io.ReadWriteCloser, error) {
				return nil, &BuildMismatchError{Running: "old", Own: "new"}
			}
		}, PhaseIncompatible},
		{"untrusted daemon socket", func(*fakeDaemon) func(context.Context) (io.ReadWriteCloser, error) {
			return func(context.Context) (io.ReadWriteCloser, error) { return nil, fmt.Errorf("dial: %w", errUntrusted) }
		}, PhaseFailed},
		{"relay refused the token", func(*fakeDaemon) func(context.Context) (io.ReadWriteCloser, error) {
			return func(context.Context) (io.ReadWriteCloser, error) { return nil, refusal{errRelayToken} }
		}, PhaseFailed},
	} {
		t.Run(test.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				rec := newRecorder()
				daemon := newFakeDaemon()
				defer close(daemon.hold)
				clock := &dialClock{start: time.Now()}
				dial := test.dial(daemon)
				counted := func(ctx context.Context) (io.ReadWriteCloser, error) {
					clock.record()
					return dial(ctx)
				}
				set := openSet(t, rec, EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: counted})
				if err := set.Connect("work"); err != nil {
					t.Fatal(err)
				}
				time.Sleep(time.Minute)
				synctest.Wait()
				requireDialTimes(t, clock.times(), 0)
				requireStamps(t, "work", rec.stampsOf("work"), []stamp{{0, PhaseConnecting}, {0, test.phase}})
			})
		})
	}
}

func TestIncompleteNativeCapabilitiesAffectOnlyThatEndpoint(t *testing.T) {
	for name, change := range map[string]func(*rpc.Capabilities){
		"missing feature":      func(c *rpc.Capabilities) { delete(c.Features, "runnerManagement") },
		"unsupported feature":  func(c *rpc.Capabilities) { c.Features["auth"] = 2 },
		"checkpoint version":   func(c *rpc.Capabilities) { c.TerminalCheckpointVersion = 2 },
		"capabilities version": func(c *rpc.Capabilities) { c.Version = 2 },
	} {
		t.Run(name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				rec := newRecorder()
				good, incomplete := newFakeDaemon(), newFakeDaemon()
				change(&incomplete.capabilities)
				set := openSet(t, rec,
					EndpointSpec{ID: "good", Label: "good", Kind: EndpointDaemon, Dial: good.Dial},
					EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: incomplete.Dial},
				)
				connectEach(t, set, "good", "work")
				requireStamps(t, "work", rec.stampsOf("work"), []stamp{{0, PhaseConnecting}, {0, PhaseIncompatible}})
				requireStamps(t, "good", rec.stampsOf("good"), []stamp{{0, PhaseConnecting}, {0, PhaseConnected}})
			})
		})
	}
}

func TestConfiguredEndpointsKeyLocalByHomeAndRemotesByName(t *testing.T) {
	home := t.TempDir()
	cfg := config.Config{Home: home + "/", Remotes: map[string]config.RemoteSpec{
		"work": {Kind: config.RemoteDaemon, URL: "https://work.example", TokenFile: home + "/work.token"},
		"box":  {Service: "docker", BoxID: "b1"},
	}}
	var got []EndpointSpec
	for _, spec := range ConfiguredEndpoints(cfg, Backend{}, NewRemotes(cfg, nil)) {
		if spec.Dial == nil {
			t.Fatalf("endpoint %s has no transport", spec.ID)
		}
		spec.Dial = nil
		got = append(got, spec)
	}
	want := []EndpointSpec{
		{ID: EndpointID(home), Label: "local", Kind: EndpointLocal},
		{ID: "box", Label: "box", Kind: EndpointBox},
		{ID: "work", Label: "work", Kind: EndpointDaemon},
	}
	if !slices.EqualFunc(got, want, func(a, b EndpointSpec) bool { return a.ID == b.ID && a.Label == b.Label && a.Kind == b.Kind }) {
		t.Fatalf("endpoints = %+v, want %+v", got, want)
	}
}

func TestEndpointSetRefusesMoreThan32EndpointsAndRepeatedIDs(t *testing.T) {
	specs := make([]EndpointSpec, MaxEndpoints+1)
	for i := range specs {
		specs[i] = EndpointSpec{ID: EndpointID(fmt.Sprintf("box%d", i))}
	}
	remotes := NewRemotes(config.Config{Home: t.TempDir()}, nil)
	if _, err := NewEndpointSet(remotes, specs, newRecorder(), ProtocolNative); err == nil {
		t.Fatal("33 endpoints accepted")
	}
	set, err := NewEndpointSet(remotes, specs[:MaxEndpoints], newRecorder(), ProtocolNative)
	if err != nil {
		t.Fatalf("32 endpoints refused: %v", err)
	}
	if err := set.SaveRemote("box32", config.RemoteSpec{Service: "docker"}); err == nil || !strings.Contains(err.Error(), "the limit is 32") {
		t.Fatalf("saving a 33rd endpoint = %v, want the limit error", err)
	}
	if err := set.SaveRemote("box3", config.RemoteSpec{Service: "docker"}); err != nil {
		t.Fatalf("editing an endpoint at the limit: %v", err)
	}
	_ = set.Close()
	if _, err := NewEndpointSet(remotes, []EndpointSpec{{ID: "work"}, {ID: "work"}}, newRecorder(), ProtocolNative); err == nil {
		t.Fatal("a repeated endpoint id accepted")
	}
}
