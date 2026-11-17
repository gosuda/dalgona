package event_test

import (
	"encoding/json/jsontext"
	"math/rand/v2"
	"reflect"
	"slices"
	"strconv"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

const (
	foldInvTurn   = "turn-1"
	foldInvTurn2  = "turn-2"
	foldInvIDA    = "thread/0/turn-1/req-1/c1"
	foldInvIDB    = "thread/0/turn-1/req-1/c2"
	foldInvIDC    = "thread/0/turn-1/req-2/c1"
	foldInvIDD    = "thread/0/turn-2/req-1/c1"
	foldInvPrintA = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
	foldInvPrintB = "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100"
)

func foldInvAssistant(turn string, calls ...string) event.AssistantMessage {
	toolCalls := make([]event.ToolCall, 0, len(calls))
	for _, id := range calls {
		toolCalls = append(toolCalls, event.ToolCall{ID: id, Name: "patch", Args: jsontext.Value(`{}`)})
	}
	return event.AssistantMessage{ItemID: "a-" + turn, TurnID: turn, StopReason: "tool_calls", ToolCalls: toolCalls}
}

func foldInvStart(turn, call string) event.ToolStarted {
	return event.ToolStarted{CallID: call, TurnID: turn, Tool: "patch", Class: "edit", Args: jsontext.Value(`{}`)}
}

func foldInvBegin(turn, call, identity, fingerprint string) event.HashlineInvocationStarted {
	return event.HashlineInvocationStarted{
		CallID: call, TurnID: turn, Identity: identity, Fingerprint: fingerprint,
		Style: "hashline-light", ContractVersion: 1, Paths: []string{"a.go"},
	}
}

func foldInvSettle(identity string) event.HashlineInvocationSettled {
	return event.HashlineInvocationSettled{Identity: identity, OK: true, Output: "applied\n"}
}

func foldInvOpen(turn string, calls ...string) []event.Event {
	events := []event.Event{event.TurnStarted{TurnID: turn}, foldInvAssistant(turn, calls...)}
	for _, call := range calls {
		events = append(events, foldInvStart(turn, call))
	}
	return events
}

func foldInvMust(t *testing.T, events ...event.Event) event.State {
	t.Helper()
	state, err := foldEvents(t, events...)
	if err != nil {
		t.Fatalf("fold of %d records: %v", len(events), err)
	}
	return state
}

func foldInvCheck(t *testing.T, state event.State, label, identity string, want event.InvocationState) {
	t.Helper()
	if got := state.Invocations[identity]; got != want {
		t.Fatalf("%s: %+v, want %+v", label, got, want)
	}
}

func TestFoldInvocationIndexesBothRecords(t *testing.T) {
	t.Parallel()
	journal := foldInvOpen(foldInvTurn, "c1")
	journal = append(journal, foldInvBegin(foldInvTurn, "c1", foldInvIDA, foldInvPrintA))
	want := event.InvocationState{CallID: "c1", TurnID: foldInvTurn, Fingerprint: foldInvPrintA, StartedSeq: uint64(len(journal))}
	state := foldInvMust(t, journal...)
	foldInvCheck(t, state, "after the start record", foldInvIDA, want)
	if state.Invocations[foldInvIDA].Settled() {
		t.Fatal("an invocation with no settled record reads as settled")
	}
	if got := state.InFlight["c1"].Invocation; got != foldInvIDA {
		t.Fatalf("in-flight call names invocation %q, want %q", got, foldInvIDA)
	}
	journal = append(journal, foldInvSettle(foldInvIDA))
	want.SettledSeq = uint64(len(journal))
	state = foldInvMust(t, journal...)
	foldInvCheck(t, state, "after the settled record", foldInvIDA, want)
	if !state.Invocations[foldInvIDA].Settled() {
		t.Fatal("a settled invocation reads as unsettled")
	}
	if state.Status != event.StatusTurnRunning {
		t.Fatalf("status after the settled record = %s, want %s", state.Status, event.StatusTurnRunning)
	}
	journal = append(journal, completedCall("c1", "none"), event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"})
	state = foldInvMust(t, journal...)
	foldInvCheck(t, state, "after the turn ends", foldInvIDA, want)
	if len(state.InFlight) != 0 || state.Status != event.StatusIdle {
		t.Fatalf("after the turn ends: in flight %v, status %s", state.InFlight, state.Status)
	}
}

func TestFoldInvocationRulesRejectIllegalRecords(t *testing.T) {
	t.Parallel()
	begin := func(call, id string) event.Event { return foldInvBegin(foldInvTurn, call, id, foldInvPrintA) }
	finish := func(call string) event.Event { return completedCall(call, "none") }
	endTurn := event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"}
	laterTurn := func(identity string) []event.Event {
		return []event.Event{
			begin("c1", foldInvIDA), foldInvSettle(foldInvIDA), finish("c1"), finish("c2"), endTurn,
			event.TurnStarted{TurnID: foldInvTurn2},
			foldInvAssistant(foldInvTurn2, "c1"), foldInvStart(foldInvTurn2, "c1"),
			foldInvBegin(foldInvTurn2, "c1", identity, foldInvPrintA),
		}
	}
	reusedCall := func(settled string) []event.Event {
		return []event.Event{
			begin("c1", foldInvIDA), finish("c1"), foldInvAssistant(foldInvTurn, "c1"), foldInvStart(foldInvTurn, "c1"),
			begin("c1", foldInvIDC), foldInvSettle(settled),
		}
	}
	cases := []struct {
		name string
		bad  []event.Event
		good []event.Event
	}{
		{
			name: "start names another turn",
			bad:  []event.Event{foldInvBegin(foldInvTurn2, "c1", foldInvIDA, foldInvPrintA)},
			good: []event.Event{begin("c1", foldInvIDA)},
		},
		{
			name: "start names an unknown call",
			bad:  []event.Event{begin("c9", foldInvIDA)},
			good: []event.Event{begin("c1", foldInvIDA)},
		},
		{
			name: "start names a call that has no tool.started",
			bad:  []event.Event{foldInvAssistant(foldInvTurn, "c3"), begin("c3", foldInvIDA)},
			good: []event.Event{foldInvAssistant(foldInvTurn, "c3"), foldInvStart(foldInvTurn, "c3"), begin("c3", foldInvIDA)},
		},
		{
			name: "start names a call that has completed",
			bad:  []event.Event{finish("c1"), begin("c1", foldInvIDA)},
			good: []event.Event{begin("c1", foldInvIDA), finish("c1")},
		},
		{
			name: "second invocation of one call",
			bad:  []event.Event{begin("c1", foldInvIDA), begin("c1", foldInvIDB)},
			good: []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDB)},
		},
		{
			name: "identity repeated for another call",
			bad:  []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDA)},
			good: []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDB)},
		},
		{
			name: "identity repeated in a later turn",
			bad:  laterTurn(foldInvIDA),
			good: laterTurn(foldInvIDD),
		},
		{
			name: "settled record names an unknown identity",
			bad:  []event.Event{begin("c1", foldInvIDA), foldInvSettle(foldInvIDC)},
			good: []event.Event{begin("c1", foldInvIDA), foldInvSettle(foldInvIDA)},
		},
		{
			name: "identity settles twice",
			bad:  []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDB), foldInvSettle(foldInvIDA), foldInvSettle(foldInvIDA)},
			good: []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDB), foldInvSettle(foldInvIDA), foldInvSettle(foldInvIDB)},
		},
		{
			name: "settled record follows the tool.completed of its call",
			bad:  []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDB), finish("c1"), foldInvSettle(foldInvIDA)},
			good: []event.Event{begin("c1", foldInvIDA), begin("c2", foldInvIDB), finish("c1"), foldInvSettle(foldInvIDB)},
		},
		{
			name: "settled record follows the turn.completed of its turn",
			bad:  []event.Event{begin("c1", foldInvIDA), finish("c1"), finish("c2"), endTurn, foldInvSettle(foldInvIDA)},
			good: []event.Event{begin("c1", foldInvIDA), foldInvSettle(foldInvIDA), finish("c1"), finish("c2"), endTurn},
		},
		{
			name: "settled record for a call id that a later call reuses",
			bad:  reusedCall(foldInvIDA),
			good: reusedCall(foldInvIDC),
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			prefix := foldInvOpen(foldInvTurn, "c1", "c2")
			last := len(test.bad) - 1
			state := foldInvMust(t, slices.Concat(prefix, test.bad[:last])...)
			seq := uint64(len(prefix) + last + 1)
			if _, err := event.Fold(state, seq, test.bad[last]); err == nil {
				t.Fatalf("Fold accepted the illegal %s", test.bad[last].Type())
			}
			foldInvMust(t, slices.Concat(prefix, test.good)...)
		})
	}
}

func TestFoldInvocationFailureKeepsTheLastGoodState(t *testing.T) {
	t.Parallel()
	journal := slices.Concat(foldInvOpen(foldInvTurn, "c1"), []event.Event{foldInvBegin(foldInvTurn, "c1", foldInvIDA, foldInvPrintA)})
	state := foldInvMust(t, journal...)
	seq := uint64(len(journal)) + 1
	second := state.Clone()
	if err := second.Apply(seq, foldInvBegin(foldInvTurn, "c1", foldInvIDB, foldInvPrintB)); err == nil {
		t.Fatal("Apply accepted a second invocation for one call")
	}
	if len(second.Invocations) != 1 || second.Invocations[foldInvIDA] != state.Invocations[foldInvIDA] {
		t.Fatalf("failed start changed the invocations: %+v", second.Invocations)
	}
	if got := second.InFlight["c1"].Invocation; got != foldInvIDA {
		t.Fatalf("failed start changed the call to invocation %q", got)
	}
	unknown := state.Clone()
	if err := unknown.Apply(seq, foldInvSettle(foldInvIDB)); err == nil {
		t.Fatal("Apply settled an unknown identity")
	}
	if unknown.Invocations[foldInvIDA].Settled() {
		t.Fatal("failed settled record settled another invocation")
	}
	control := state.Clone()
	if err := control.Apply(seq, foldInvSettle(foldInvIDA)); err != nil {
		t.Fatalf("Apply rejected the settled record of the started identity: %v", err)
	}
}

func TestFoldInvocationAcceptsBothCrashShapes(t *testing.T) {
	t.Parallel()
	started := slices.Concat(foldInvOpen(foldInvTurn, "c1"), []event.Event{foldInvBegin(foldInvTurn, "c1", foldInvIDA, foldInvPrintA)})
	t.Run("started record without a settled record", func(t *testing.T) {
		t.Parallel()
		foldInvInterruptedCall(t, started)
	})
	t.Run("settled record without a completed call", func(t *testing.T) {
		t.Parallel()
		foldInvReplayedCompletion(t, started)
	})
}

func foldInvInterruptedCall(t *testing.T, started []event.Event) {
	t.Helper()
	interrupted := event.ToolCompleted{CallID: "c1", OK: false, Output: "interrupted", Recovered: "interrupted"}
	journal := slices.Concat(started, []event.Event{interrupted, event.TurnCompleted{TurnID: foldInvTurn, Status: "interrupted"}})
	state := foldInvMust(t, journal...)
	got := state.Invocations[foldInvIDA]
	if got.Settled() || got.StartedSeq != uint64(len(started)) {
		t.Fatalf("invocation after an unsettled call completes = %+v", got)
	}
	if len(state.InFlight) != 0 || state.Status != event.StatusIdle {
		t.Fatalf("unsettled call left in flight %v with status %s", state.InFlight, state.Status)
	}
}

func foldInvReplayedCompletion(t *testing.T, started []event.Event) {
	t.Helper()
	journal := slices.Concat(started, []event.Event{foldInvSettle(foldInvIDA)})
	state := foldInvMust(t, journal...)
	if _, running := state.InFlight["c1"]; !running {
		t.Fatal("the settled record completed its call")
	}
	seq := uint64(len(journal)) + 1
	if _, err := event.Fold(state, seq, event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"}); err == nil {
		t.Fatal("Fold ended the turn while a settled call had no tool.completed")
	}
	replayed := event.ToolCompleted{CallID: "c1", OK: true, Output: "applied\n", Recovered: "replayed"}
	next, err := event.Fold(state, seq, replayed)
	if err != nil {
		t.Fatalf("Fold rejected the replayed completion: %v", err)
	}
	if _, running := next.InFlight["c1"]; running {
		t.Fatal("the replayed completion left the call in flight")
	}
	if got := next.Invocations[foldInvIDA].SettledSeq; got != uint64(len(journal)) {
		t.Fatalf("settled sequence after the completion = %d, want %d", got, len(journal))
	}
}

func TestFoldInvocationKeysEachCallByIdentity(t *testing.T) {
	t.Parallel()
	var journal []event.Event
	add := func(e event.Event) uint64 {
		journal = append(journal, e)
		return uint64(len(journal))
	}
	add(event.TurnStarted{TurnID: foldInvTurn})
	add(foldInvAssistant(foldInvTurn, "c1", "c2"))
	add(foldInvStart(foldInvTurn, "c1"))
	add(foldInvStart(foldInvTurn, "c2"))
	startA := add(foldInvBegin(foldInvTurn, "c1", foldInvIDA, foldInvPrintA))
	startB := add(foldInvBegin(foldInvTurn, "c2", foldInvIDB, foldInvPrintB))
	settleB := add(foldInvSettle(foldInvIDB))
	settleA := add(foldInvSettle(foldInvIDA))
	add(completedCall("c1", "none"))
	add(completedCall("c2", "none"))
	add(event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"})
	add(event.TurnStarted{TurnID: foldInvTurn2})
	add(foldInvAssistant(foldInvTurn2, "c1"))
	add(foldInvStart(foldInvTurn2, "c1"))
	startD := add(foldInvBegin(foldInvTurn2, "c1", foldInvIDD, foldInvPrintB))
	state := foldInvMust(t, journal...)
	want := map[string]event.InvocationState{
		foldInvIDA: {CallID: "c1", TurnID: foldInvTurn, Fingerprint: foldInvPrintA, StartedSeq: startA, SettledSeq: settleA},
		foldInvIDB: {CallID: "c2", TurnID: foldInvTurn, Fingerprint: foldInvPrintB, StartedSeq: startB, SettledSeq: settleB},
		foldInvIDD: {CallID: "c1", TurnID: foldInvTurn2, Fingerprint: foldInvPrintB, StartedSeq: startD},
	}
	if !reflect.DeepEqual(state.Invocations, want) {
		t.Fatalf("invocations = %+v, want %+v", state.Invocations, want)
	}
	if got := state.InFlight["c1"].Invocation; got != foldInvIDD {
		t.Fatalf("call c1 of turn-2 names invocation %q, want %q", got, foldInvIDD)
	}
}

func TestFoldInvocationDoesNotMutateInput(t *testing.T) {
	t.Parallel()
	open := foldInvOpen(foldInvTurn, "c1")
	running := foldInvMust(t, open...)
	started, err := event.Fold(running, uint64(len(open))+1, foldInvBegin(foldInvTurn, "c1", foldInvIDA, foldInvPrintA))
	if err != nil {
		t.Fatalf("Fold of the start record: %v", err)
	}
	if len(running.Invocations) != 0 || running.InFlight["c1"].Invocation != "" {
		t.Fatalf("start record changed its input: %+v, %+v", running.Invocations, running.InFlight["c1"])
	}
	settled, err := event.Fold(started, uint64(len(open))+2, foldInvSettle(foldInvIDA))
	if err != nil {
		t.Fatalf("Fold of the settled record: %v", err)
	}
	if started.Invocations[foldInvIDA].Settled() {
		t.Fatal("settled record changed its input")
	}
	if !settled.Invocations[foldInvIDA].Settled() {
		t.Fatal("settled record did not settle the returned state")
	}
}

func TestFoldInvocationFoldsDecodedRecords(t *testing.T) {
	t.Parallel()
	journal := slices.Concat(foldInvOpen(foldInvTurn, "c1"), []event.Event{
		foldInvBegin(foldInvTurn, "c1", foldInvIDA, foldInvPrintA),
		foldInvSettle(foldInvIDA),
		event.ToolCompleted{CallID: "c1", OK: true, Output: "applied\n", Recovered: "replayed"},
		event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"},
	})
	decoded := make([]event.Event, len(journal))
	for i, record := range journal {
		raw, err := event.Encode(record)
		if err != nil {
			t.Fatalf("Encode(%s): %v", record.Type(), err)
		}
		switch record.Type() {
		case event.TypeHashlineInvocationStarted:
			raw = []byte(`{"type":"hashline.invocation_started","call_id":"c1","turn_id":"turn-1","identity":"thread/0/turn-1/req-1/c1","fingerprint":"00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff","style":"hashline-light","contract_version":1,"paths":["a.go"]}`)
		case event.TypeHashlineInvocationSettled:
			raw = []byte(`{"type":"hashline.invocation_settled","identity":"thread/0/turn-1/req-1/c1","ok":true,"output":"applied\n"}`)
		}
		if decoded[i], err = event.Decode(raw); err != nil {
			t.Fatalf("Decode(%s): %v", record.Type(), err)
		}
	}
	got := foldInvMust(t, decoded...)
	want := map[string]event.InvocationState{
		foldInvIDA: {CallID: "c1", TurnID: foldInvTurn, Fingerprint: foldInvPrintA, StartedSeq: 4, SettledSeq: 5},
	}
	if len(got.Invocations) != 1 || !got.Invocations[foldInvIDA].Settled() {
		t.Fatalf("decoded journal gave invocations %+v", got.Invocations)
	}
	if !reflect.DeepEqual(got.Invocations, want) {
		t.Fatalf("decoded journal gave %+v, want journal positions %+v", got.Invocations, want)
	}
}

func TestFoldWithoutInvocationRecordsLeavesInvocationStateEmpty(t *testing.T) {
	t.Parallel()
	open := foldInvOpen(foldInvTurn, "c1")
	running := foldInvMust(t, open...)
	if running.Invocations != nil || running.InFlight["c1"].Invocation != "" {
		t.Fatalf("a journal without invocation records produced %+v, %+v", running.Invocations, running.InFlight["c1"])
	}
	done := foldInvMust(t, append(open, completedCall("c1", "none"), event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"})...)
	if done.Invocations != nil {
		t.Fatalf("a finished journal without invocation records produced %+v", done.Invocations)
	}
}

func foldInvScripts() [][]string {
	return [][]string{{"complete"}, {"begin", "complete"}, {"begin", "settle", "complete"}}
}

type foldInvLive struct {
	call     string
	identity string
	steps    []string
}

func foldInvStepEvent(item foldInvLive, step string) event.Event {
	switch step {
	case "begin":
		return foldInvBegin(foldInvTurn, item.call, item.identity, foldInvPrintA)
	case "settle":
		return foldInvSettle(item.identity)
	default:
		return completedCall(item.call, "none")
	}
}

func foldInvRecord(model map[string]event.InvocationState, item foldInvLive, step string, seq uint64) {
	entry := model[item.identity]
	switch step {
	case "begin":
		model[item.identity] = event.InvocationState{CallID: item.call, TurnID: foldInvTurn, Fingerprint: foldInvPrintA, StartedSeq: seq}
	case "settle":
		entry.SettledSeq = seq
		model[item.identity] = entry
	}
}

func foldInvRandomJournal(r *rand.Rand, calls int) ([]event.Event, map[string]event.InvocationState) {
	names := make([]string, calls)
	live := make([]foldInvLive, calls)
	for i := range calls {
		names[i] = "c" + strconv.Itoa(i)
		scripts := foldInvScripts()
		live[i] = foldInvLive{call: names[i], identity: "thread/0/turn-1/req-1/" + names[i], steps: scripts[r.IntN(len(scripts))]}
	}
	events := foldInvOpen(foldInvTurn, names...)
	model := make(map[string]event.InvocationState)
	for len(live) > 0 {
		pick := r.IntN(len(live))
		item := live[pick]
		events = append(events, foldInvStepEvent(item, item.steps[0]))
		foldInvRecord(model, item, item.steps[0], uint64(len(events)))
		live[pick].steps = item.steps[1:]
		if len(live[pick].steps) == 0 {
			live = slices.Delete(live, pick, pick+1)
		}
	}
	return append(events, event.TurnCompleted{TurnID: foldInvTurn, Status: "completed"}), model
}

func foldInvSeedCounts(t *testing.T, seed uint64) (settled, unsettled int) {
	t.Helper()
	r := rand.New(rand.NewPCG(seed, 7))
	events, model := foldInvRandomJournal(r, 1+r.IntN(5))
	state, err := foldEvents(t, events...)
	if err != nil {
		t.Fatalf("seed %d: fold rejected a legal journal: %v", seed, err)
	}
	if len(state.Invocations) != len(model) {
		t.Fatalf("seed %d: %d invocations, model has %d", seed, len(state.Invocations), len(model))
	}
	for identity, want := range model {
		if got := state.Invocations[identity]; got != want {
			t.Fatalf("seed %d: invocation %s = %+v, want %+v", seed, identity, got, want)
		}
		if want.Settled() {
			settled++
		} else {
			unsettled++
		}
	}
	return settled, unsettled
}

func TestFoldInvocationMatchesAJournalPositionModelOverSeededOrders(t *testing.T) {
	t.Parallel()
	var settled, unsettled int
	for seed := range uint64(200) {
		seedSettled, seedUnsettled := foldInvSeedCounts(t, seed)
		settled += seedSettled
		unsettled += seedUnsettled
	}
	if settled == 0 || unsettled == 0 {
		t.Fatalf("seeds exercised %d settled and %d unsettled invocations, want both", settled, unsettled)
	}
}
