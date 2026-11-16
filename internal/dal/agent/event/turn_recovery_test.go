package event_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func TestTurnRecoveryUsesTheSpecifiedMembers(t *testing.T) {
	t.Parallel()
	decoded, err := event.Decode([]byte(`{"type":"turn.recovery","turn_id":"t1","attempt":2}`))
	if err != nil {
		t.Fatalf("decode the specified record: %v", err)
	}
	want := event.TurnRecovery{TurnID: "t1", Attempt: 2}
	if decoded != event.Event(want) {
		t.Fatalf("decoded %+v, want %+v", decoded, want)
	}
	raw, err := event.Encode(want)
	if err != nil {
		t.Fatal(err)
	}
	again, err := event.Decode(raw)
	if err != nil || again != event.Event(want) {
		t.Fatalf("round trip gave %+v, %v", again, err)
	}
}

func TestDecodeRejectsInvalidTurnRecovery(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		raw  string
	}{
		{name: "missing turn_id", raw: `{"type":"turn.recovery","attempt":1}`},
		{name: "missing attempt", raw: `{"type":"turn.recovery","turn_id":"t"}`},
		{name: "zero attempt", raw: `{"type":"turn.recovery","turn_id":"t","attempt":0}`},
		{name: "negative attempt", raw: `{"type":"turn.recovery","turn_id":"t","attempt":-1}`},
		{name: "text attempt", raw: `{"type":"turn.recovery","turn_id":"t","attempt":"1"}`},
	}
	for _, test := range cases {
		if _, err := event.Decode([]byte(test.raw)); err == nil {
			t.Fatalf("%s: decode accepted the record", test.name)
		}
	}
	if _, err := event.Encode(event.TurnRecovery{Attempt: 1}); err == nil {
		t.Fatal("encode accepted a record without turn_id")
	}
	if _, err := event.Decode([]byte(`{"type":"turn.recovery","turn_id":"t","attempt":1}`)); err != nil {
		t.Fatalf("control record rejected: %v", err)
	}
}

func recoveryFold(t *testing.T, events ...event.Event) (event.State, error) {
	t.Helper()
	open := []event.Event{
		event.SessionCreated{ID: "s", Cwd: "/w", Model: "m", Approval: "auto", Sandbox: "off", Runner: "local"},
		event.TurnStarted{TurnID: "t"},
	}
	return foldEvents(t, append(open, events...)...)
}

func TestFoldCountsRecoveryAttemptsOfTheOpenTurn(t *testing.T) {
	t.Parallel()
	state, err := recoveryFold(t)
	if err != nil {
		t.Fatal(err)
	}
	if got := state.RecoveryAttempts("t"); got != 0 {
		t.Fatalf("attempts before any record = %d, want 0", got)
	}
	seq := uint64(3)
	for attempt := 1; attempt <= 3; attempt++ {
		state, err = event.Fold(state, seq, event.TurnRecovery{TurnID: "t", Attempt: attempt})
		if err != nil {
			t.Fatalf("attempt %d: %v", attempt, err)
		}
		seq++
		if got := state.RecoveryAttempts("t"); got != attempt {
			t.Fatalf("attempts after record %d = %d", attempt, got)
		}
	}
	if got := state.RecoveryAttempts("other"); got != 0 {
		t.Fatalf("attempts of another turn = %d, want 0", got)
	}
}

func TestFoldKeepsRecoveryCountAcrossProgress(t *testing.T) {
	t.Parallel()
	state, err := recoveryFold(t,
		event.TurnRecovery{TurnID: "t", Attempt: 1},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "more", Kind: "steer"},
		event.ModelRetry{TurnID: "t", Attempt: 1, Verdict: "retry", DelayMS: 1},
	)
	if err != nil {
		t.Fatal(err)
	}
	if got := state.RecoveryAttempts("t"); got != 1 {
		t.Fatalf("attempts after progress = %d, want 1", got)
	}
	state, err = event.Fold(state, 6, event.TurnRecovery{TurnID: "t", Attempt: 2})
	if err != nil {
		t.Fatalf("attempt 2 after progress: %v", err)
	}
	if got := state.RecoveryAttempts("t"); got != 2 {
		t.Fatalf("attempts = %d, want 2", got)
	}
}

func TestFoldRejectsMisorderedRecoveryRecords(t *testing.T) {
	t.Parallel()
	one := event.TurnRecovery{TurnID: "t", Attempt: 1}
	cases := []struct {
		name   string
		events []event.Event
	}{
		{name: "first attempt is 2", events: []event.Event{event.TurnRecovery{TurnID: "t", Attempt: 2}}},
		{name: "repeated attempt", events: []event.Event{one, one}},
		{name: "skipped attempt", events: []event.Event{one, event.TurnRecovery{TurnID: "t", Attempt: 3}}},
		{name: "another turn", events: []event.Event{event.TurnRecovery{TurnID: "u", Attempt: 1}}},
		{name: "no open turn", events: []event.Event{event.TurnCompleted{TurnID: "t", Status: "failed"}, one}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if _, err := recoveryFold(t, test.events...); err == nil {
				t.Fatal("fold accepted the records")
			}
		})
	}
	if _, err := recoveryFold(t, one, event.TurnRecovery{TurnID: "t", Attempt: 2}); err != nil {
		t.Fatalf("control sequence rejected: %v", err)
	}
}

func TestTurnCompletedEndsTheRecoveryCount(t *testing.T) {
	t.Parallel()
	state, err := recoveryFold(t,
		event.TurnRecovery{TurnID: "t", Attempt: 1},
		event.TurnRecovery{TurnID: "t", Attempt: 2},
		event.TurnCompleted{TurnID: "t", Status: "failed", Error: "gave up"},
		event.TurnStarted{TurnID: "u"},
	)
	if err != nil {
		t.Fatal(err)
	}
	if got := state.RecoveryAttempts("t"); got != 0 {
		t.Fatalf("attempts of the completed turn = %d, want 0", got)
	}
	state, err = event.Fold(state, 7, event.TurnRecovery{TurnID: "u", Attempt: 1})
	if err != nil {
		t.Fatalf("first attempt of the next turn: %v", err)
	}
	if got := state.RecoveryAttempts("u"); got != 1 {
		t.Fatalf("attempts of the next turn = %d, want 1", got)
	}
}

func TestFoldOfRecoveryDoesNotMutateItsInput(t *testing.T) {
	t.Parallel()
	original, err := recoveryFold(t, event.TurnRecovery{TurnID: "t", Attempt: 1})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = event.Fold(original, 4, event.TurnRecovery{TurnID: "t", Attempt: 2}); err != nil {
		t.Fatal(err)
	}
	if got := original.RecoveryAttempts("t"); got != 1 {
		t.Fatalf("Fold changed its input to %d attempts, want 1", got)
	}
}

func TestTurnEffortRoundTripRequiresTheOpenTurn(t *testing.T) {
	t.Parallel()
	for _, effort := range []string{"max", ""} {
		requireEffortRoundTrip(t, effort)
	}
	if _, err := event.Encode(event.TurnEffort{Effort: "max"}); err == nil {
		t.Fatal("effort without turn id accepted")
	}
	for _, tail := range [][]event.Event{
		{event.TurnEffort{TurnID: "other"}},
		{event.TurnCompleted{TurnID: "t", Status: "interrupted"}, event.TurnEffort{TurnID: "t"}},
	} {
		if _, err := recoveryFold(t, tail...); err == nil {
			t.Fatalf("out-of-turn effort accepted: %#v", tail)
		}
	}
}

func requireEffortRoundTrip(t *testing.T, effort string) {
	t.Helper()
	raw, err := event.Encode(event.TurnEffort{TurnID: "t", Effort: effort})
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := event.Decode(raw)
	if err != nil {
		t.Fatal(err)
	}
	value, ok := decoded.(event.TurnEffort)
	if !ok || value.TurnID != "t" || value.Effort != effort {
		t.Fatalf("round trip = %#v, want turn t effort %q", decoded, effort)
	}
	if _, err := recoveryFold(t, decoded); err != nil {
		t.Fatalf("resolved effort %q rejected: %v", effort, err)
	}
}
