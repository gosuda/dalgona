package event_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func foldAll(t *testing.T, state event.State, seq uint64, events ...event.Event) event.State {
	t.Helper()
	out := state
	var err error
	for i, e := range events {
		out, err = event.Fold(out, seq+uint64(i), e)
		if err != nil {
			t.Fatalf("fold %s: %v", e.Type(), err)
		}
	}
	return out
}

func roundTripMailEvent(t *testing.T, original event.Event) {
	t.Helper()
	encoded, err := event.Encode(original)
	if err != nil {
		t.Fatalf("encode %s: %v", original.Type(), err)
	}
	decoded, err := event.Decode(encoded)
	if err != nil {
		t.Fatalf("decode %s: %v", original.Type(), err)
	}
	if decoded.Type() != original.Type() {
		t.Fatalf("type = %s, want %s", decoded.Type(), original.Type())
	}
}

func foldMailLifecycle(t *testing.T, outgoing, received event.MailMessage) event.State {
	t.Helper()
	return foldAll(t, event.State{}, 1,
		event.TurnStarted{TurnID: "turn-1"},
		event.MailOutgoing{Message: outgoing},
		event.MailReceived{Message: received},
		event.MailConsumed{IDs: []string{received.ID}, TurnID: "turn-1", Hop: 2},
		event.MailSettled{ID: outgoing.ID, Outcome: "delivered", RecipientTurnID: "turn-1"},
		event.MailRunSet{Enabled: true},
		event.MailAutoStarted{TurnID: "turn-2"},
		event.MailHopAdvanced{TurnID: "turn-1", Hop: 4},
		event.MailHeld{Reason: "turn_failed_or_interrupted"},
		event.MailHumanReset{TurnID: "turn-3"},
	)
}

func assertMailLifecycleFolded(t *testing.T, state event.State, outgoing, received event.MailMessage) {
	t.Helper()
	if state.Mail == nil {
		t.Fatal("mail state is nil after fold")
	}
	if got := state.Mail.Pairs[event.MailPairKey(outgoing.From, outgoing.To)].LastSeq; got != 1 {
		t.Fatalf("pair last seq = %d, want 1", got)
	}
	if !state.Mail.RunOnMail || state.Mail.AutoTurns != 0 {
		t.Fatalf("run state = %+v, want run-on with auto turns cleared by reset", state.Mail)
	}
	if state.Mail.Held || state.Mail.HeldReason != "" {
		t.Fatalf("held not cleared by human reset: %+v", state.Mail)
	}
	assertMailLifecycleEntries(t, state, outgoing, received)
}

func assertMailLifecycleEntries(t *testing.T, state event.State, outgoing, received event.MailMessage) {
	t.Helper()
	entry, ok := state.Mail.Messages[received.ID]
	if !ok || entry.Outcome != event.MailOutcomeDelivered || entry.ConsumedTurn != "turn-1" {
		t.Fatalf("received entry = %+v, want delivered in turn-1", entry)
	}
	out, ok := state.Mail.Messages[outgoing.ID]
	if !ok || out.Outcome != event.MailOutcomeDelivered {
		t.Fatalf("outgoing entry = %+v, want delivered", out)
	}
}

func TestMailCodecAndFold(t *testing.T) {
	t.Parallel()
	outgoing := mailMessage("mail-"+strings.Repeat("a", 64), 1)
	outgoing.ClockMS = 1700000000000
	received := mailMessage("mail-"+strings.Repeat("b", 64), 3)
	received.From = mailAddress("s9", "main")
	received.To = mailAddress("s1", "main")
	received.ClockMS = 1700000000001
	received.BucketTokens = 2
	received.BucketRefillMS = 1700000002000
	for _, original := range []event.Event{
		event.MailOutgoing{Message: outgoing},
		event.MailReceived{Message: received},
		event.MailConsumed{IDs: []string{received.ID}, TurnID: "turn-1", Hop: 2},
		event.MailSettled{ID: outgoing.ID, Outcome: "delivered", RecipientTurnID: "turn-1"},
		event.MailRunSet{Enabled: true},
		event.MailAutoStarted{TurnID: "turn-2"},
		event.MailHopAdvanced{TurnID: "turn-1", Hop: 4},
		event.MailHeld{Reason: "turn_failed_or_interrupted"},
		event.MailHumanReset{TurnID: "turn-3"},
	} {
		roundTripMailEvent(t, original)
	}
	state := foldMailLifecycle(t, outgoing, received)
	assertMailLifecycleFolded(t, state, outgoing, received)
}

func TestMailBranchDoesNotRewindDedup(t *testing.T) {
	t.Parallel()
	first := mailMessage("mail-"+strings.Repeat("c", 64), 1)
	state := foldAll(t, event.State{}, 1, event.MailOutgoing{Message: first})
	redelivered := foldAll(t, state, 2, event.MailOutgoing{Message: first})
	if got := redelivered.Mail.Pairs[event.MailPairKey(first.From, first.To)].LastSeq; got != 1 {
		t.Fatalf("pair last seq after redelivery = %d, want 1", got)
	}
	second := mailMessage("mail-"+strings.Repeat("d", 64), 2)
	second.From, second.To = first.From, first.To
	advanced := foldAll(t, redelivered, 3, event.MailOutgoing{Message: second})
	if got := advanced.Mail.Pairs[event.MailPairKey(first.From, first.To)].LastSeq; got != 2 {
		t.Fatalf("pair last seq = %d, want 2", got)
	}
	if _, err := event.Fold(advanced, 4, event.MailOutgoing{Message: first}); err != nil {
		t.Fatalf("old branch redelivery must dedup, got %v", err)
	}
	changed := first
	changed.Text = "different body"
	if _, err := event.Fold(advanced, 5, event.MailOutgoing{Message: changed}); err == nil {
		t.Fatal("changed payload under one id must conflict")
	}
	regressed := mailMessage("mail-"+strings.Repeat("e", 64), 1)
	regressed.From, regressed.To = first.From, first.To
	if _, err := event.Fold(advanced, 6, event.MailOutgoing{Message: regressed}); err == nil {
		t.Fatal("regressed pair seq with new id must fail")
	}
}

func TestMailReceiverAdmissionPersists(t *testing.T) {
	t.Parallel()
	received := mailMessage("mail-"+strings.Repeat("f", 64), 7)
	received.ClockMS = 1700000000009
	received.BucketTokens = 1
	received.BucketRefillMS = 1700000001009
	state := foldAll(t, event.State{}, 1, event.MailReceived{Message: received})
	entry := state.Mail.Messages[received.ID]
	if entry.Message.ClockMS != 1700000000009 || entry.Message.BucketTokens != 1 || entry.Message.BucketRefillMS != 1700000001009 {
		t.Fatalf("admission metadata lost in fold: %+v", entry.Message)
	}
	encoded, err := event.Encode(event.MailReceived{Message: received})
	if err != nil {
		t.Fatalf("encode received: %v", err)
	}
	decoded, err := event.Decode(encoded)
	if err != nil {
		t.Fatalf("decode received: %v", err)
	}
	restored := foldAll(t, event.State{}, 1, decoded)
	again, ok := restored.Mail.Messages[received.ID]
	if !ok {
		t.Fatal("received message missing after refold")
	}
	if again.Message != entry.Message {
		t.Fatalf("admission changed across refold: %+v vs %+v", again.Message, entry.Message)
	}
	if _, err := event.Fold(restored, 2, event.MailReceived{Message: received}); err != nil {
		t.Fatalf("duplicate admission must dedup, got %v", err)
	}
}
