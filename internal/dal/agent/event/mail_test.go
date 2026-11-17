package event_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func mailAddress(session, agent string) event.MailAddress {
	return event.MailAddress{SessionID: session, AgentID: agent}
}

func mailMessage(id string, pairSeq uint64) event.MailMessage {
	return event.MailMessage{
		ID: id, From: mailAddress("s1", "main"), To: mailAddress("s1", "g-1/n-1"),
		PairSeq: pairSeq, Text: "peer text", ReplyTo: "", Hop: 1, AcceptedMS: 1700000000000,
	}
}

func TestMailCodecRoundTrip(t *testing.T) {
	t.Parallel()
	received := mailMessage("mail-"+strings.Repeat("a", 64), 3)
	received.ClockMS = 1700000000001
	received.BucketTokens = 2
	received.BucketRefillMS = 1700000002000
	outgoing := mailMessage("mail-"+strings.Repeat("b", 64), 1)
	outgoing.ClockMS = 1700000000002
	reply := mailMessage("mail-"+strings.Repeat("c", 64), 2)
	reply.ReplyTo = "mail-" + strings.Repeat("a", 64)
	candidates := []event.Event{
		event.MailOutgoing{Message: outgoing},
		event.MailReceived{Message: received},
		event.MailReceived{Message: reply},
		event.MailConsumed{IDs: []string{received.ID, reply.ID}, TurnID: "t1", Hop: 2},
		event.MailSettled{ID: outgoing.ID, Outcome: "delivered", RecipientTurnID: "rt1"},
		event.MailSettled{ID: outgoing.ID, Outcome: "refused", Reason: "recipient finished"},
		event.MailSettled{ID: outgoing.ID, Outcome: "queued"},
		event.MailRunSet{Enabled: true},
		event.MailRunSet{Enabled: false},
		event.MailAutoStarted{TurnID: "t2"},
		event.MailHumanReset{TurnID: "t3"},
		event.MailHopAdvanced{TurnID: "t4", Hop: 5},
		event.MailHeld{Reason: "turn_failed_or_interrupted"},
		event.TurnStarted{TurnID: "t5", MailHop: 3},
		event.ChildSpawned{CallID: "c", ChildID: "ch", Runner: "local", MailHop: 4},
	}
	for _, original := range candidates {
		assertRoundTrip(t, 0, original)
	}
}

func mustEncodeMail(t *testing.T, what string, original event.Event) []byte {
	t.Helper()
	encoded, err := event.Encode(original)
	if err != nil {
		t.Fatalf("Encode %s: %v", what, err)
	}
	return encoded
}

func assertEncodingOmits(t *testing.T, what string, encoded []byte, members ...string) {
	t.Helper()
	for _, member := range members {
		if strings.Contains(string(encoded), `"`+member+`"`) {
			t.Fatalf("%s encoding %s carries %s", what, encoded, member)
		}
	}
}

func decodeMailReceived(t *testing.T, encoded []byte) event.MailReceived {
	t.Helper()
	decoded, err := event.Decode(encoded)
	if err != nil {
		t.Fatalf("Decode received: %v", err)
	}
	message, ok := decoded.(event.MailReceived)
	if !ok {
		t.Fatalf("decoded %T, want MailReceived", decoded)
	}
	return message
}

func TestMailCodecOmitsEmptyAdmission(t *testing.T) {
	t.Parallel()
	outgoing := mustEncodeMail(t, "outgoing", event.MailOutgoing{Message: mailMessage("mail-"+strings.Repeat("d", 64), 1)})
	assertEncodingOmits(t, "outgoing", outgoing, "bucket_tokens", "bucket_refill_ms", "clock_ms", "reply_to")
	received := mustEncodeMail(t, "received", event.MailReceived{Message: mailMessage("mail-"+strings.Repeat("e", 64), 1)})
	assertEncodingOmits(t, "received", received, "bucket_tokens", "bucket_refill_ms")
	if !strings.Contains(string(received), `"accepted_ms"`) {
		t.Fatalf("received encoding %s lacks accepted_ms", received)
	}
	message := decodeMailReceived(t, received)
	if message.Message.Hop != 1 || message.Message.PairSeq != 1 {
		t.Fatalf("decoded message = %#v", message.Message)
	}
}

func TestMailValidateRejectsMalformed(t *testing.T) {
	t.Parallel()
	good := mailMessage("mail-"+strings.Repeat("f", 64), 1)
	cases := []struct {
		name  string
		event event.Event
	}{
		{"bad id", event.MailReceived{Message: func() event.MailMessage { m := good; m.ID = "x"; return m }()}},
		{"empty from session", event.MailReceived{Message: func() event.MailMessage { m := good; m.From.SessionID = ""; return m }()}},
		{"empty to agent", event.MailReceived{Message: func() event.MailMessage { m := good; m.To.AgentID = ""; return m }()}},
		{"zero pair seq", event.MailReceived{Message: func() event.MailMessage { m := good; m.PairSeq = 0; return m }()}},
		{"oversize text", event.MailReceived{Message: func() event.MailMessage { m := good; m.Text = strings.Repeat("x", event.MaxMailTextBytes+1); return m }()}},
		{"bad reply_to", event.MailReceived{Message: func() event.MailMessage { m := good; m.ReplyTo = "nope"; return m }()}},
		{"hop over limit", event.MailReceived{Message: func() event.MailMessage { m := good; m.Hop = 9; return m }()}},
		{"consumed no ids", event.MailConsumed{TurnID: "t", Hop: 1}},
		{"consumed bad hop", event.MailConsumed{IDs: []string{good.ID}, TurnID: "t", Hop: 9}},
		{"consumed bad id", event.MailConsumed{IDs: []string{"x"}, TurnID: "t", Hop: 1}},
		{"settled bad outcome", event.MailSettled{ID: good.ID, Outcome: "maybe"}},
		{"settled delivered no turn", event.MailSettled{ID: good.ID, Outcome: "delivered"}},
		{"hop advanced zero", event.MailHopAdvanced{TurnID: "t", Hop: 0}},
		{"hop advanced over", event.MailHopAdvanced{TurnID: "t", Hop: 9}},
		{"held bad reason", event.MailHeld{Reason: "other"}},
		{"auto started no turn", event.MailAutoStarted{}},
		{"human reset no turn", event.MailHumanReset{}},
		{"turn mail_hop over", event.TurnStarted{TurnID: "t", MailHop: 9}},
		{"child mail_hop over", event.ChildSpawned{CallID: "c", ChildID: "ch", Runner: "local", MailHop: 9}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if err := test.event.Validate(); err == nil {
				t.Fatalf("%s validated", test.name)
			}
			if _, err := event.Encode(test.event); err == nil {
				t.Fatalf("%s encoded", test.name)
			}
		})
	}
}

func TestMailDecodeRejectsMalformed(t *testing.T) {
	t.Parallel()
	inputs := []string{
		`{"type":"mail.received","id":"mail-x"}`,
		`{"type":"mail.consumed","ids":["mail-x"],"turn_id":"t","hop":300}`,
		`{"type":"mail.settled","id":"mail-x","outcome":"delivered"}`,
		`{"type":"mail.held","reason":"bogus"}`,
	}
	for _, input := range inputs {
		if _, err := event.Decode([]byte(input)); err == nil {
			t.Fatalf("Decode(%s) succeeded", input)
		}
	}
}

func TestMailFoldDedupAndConflict(t *testing.T) {
	t.Parallel()
	first := mailMessage("mail-"+strings.Repeat("1", 64), 1)
	second := mailMessage("mail-"+strings.Repeat("2", 64), 2)
	state, err := foldEvents(t,
		event.MailReceived{Message: first},
		event.MailReceived{Message: first},
		event.MailReceived{Message: second},
	)
	if err != nil {
		t.Fatalf("Fold received: %v", err)
	}
	if state.Mail == nil || len(state.Mail.Messages) != 2 {
		t.Fatalf("mail state = %#v", state.Mail)
	}
	changed := first
	changed.Text = "different"
	if _, err := event.Fold(state, 4, event.MailReceived{Message: changed}); err == nil {
		t.Fatal("Fold accepted a conflicting payload under one id")
	}
}

func TestMailFoldOutgoingPairOrder(t *testing.T) {
	t.Parallel()
	first := mailMessage("mail-"+strings.Repeat("3", 64), 1)
	second := mailMessage("mail-"+strings.Repeat("4", 64), 2)
	state, err := foldEvents(t,
		event.MailOutgoing{Message: first},
		event.MailOutgoing{Message: second},
	)
	if err != nil {
		t.Fatalf("Fold outgoing: %v", err)
	}
	if state.Mail.Pairs[event.MailPairKey(first.From, first.To)].LastSeq != 2 {
		t.Fatalf("pair state = %#v", state.Mail.Pairs)
	}
	stale := mailMessage("mail-"+strings.Repeat("5", 64), 1)
	if _, err := event.Fold(state, 3, event.MailOutgoing{Message: stale}); err == nil {
		t.Fatal("Fold accepted an out-of-order pair sequence")
	}
	other := stale
	other.To = mailAddress("s2", "main")
	if _, err := event.Fold(state, 3, event.MailOutgoing{Message: other}); err != nil {
		t.Fatalf("Fold independent pair: %v", err)
	}
}

func foldMailLifecycleState(t *testing.T, inbox, sent event.MailMessage) event.State {
	t.Helper()
	state, err := foldEvents(t,
		event.MailReceived{Message: inbox},
		event.TurnStarted{TurnID: "t1", MailHop: 1},
		event.MailHopAdvanced{TurnID: "t1", Hop: 3},
		event.MailConsumed{IDs: []string{inbox.ID}, TurnID: "t1", Hop: 3},
		event.TurnCompleted{TurnID: "t1", Status: "completed"},
		event.MailOutgoing{Message: sent},
		event.MailSettled{ID: sent.ID, Outcome: "queued"},
		event.MailSettled{ID: sent.ID, Outcome: "delivered", RecipientTurnID: "rt"},
		event.MailRunSet{Enabled: true},
		event.MailAutoStarted{TurnID: "t2"},
		event.MailAutoStarted{TurnID: "t3"},
		event.MailHeld{Reason: "turn_failed_or_interrupted"},
	)
	if err != nil {
		t.Fatalf("Fold mail lifecycle: %v", err)
	}
	return state
}

func assertMailLifecycleFlags(t *testing.T, state event.State, inbox event.MailMessage) {
	t.Helper()
	mail := state.Mail
	if mail == nil {
		t.Fatal("state.Mail is nil")
	}
	if !mail.RunOnMail || mail.AutoTurns != 2 || !mail.Held || mail.HeldReason != "turn_failed_or_interrupted" {
		t.Fatalf("mail flags = %#v", mail)
	}
	if mail.TurnHops["t1"] != 3 {
		t.Fatalf("turn hops = %#v", mail.TurnHops)
	}
	entry := mail.Messages[inbox.ID]
	if entry.Outcome != event.MailOutcomeDelivered || entry.ConsumedTurn != "t1" {
		t.Fatalf("consumed entry = %#v", entry)
	}
}

func assertMailHumanReset(t *testing.T, state event.State, inbox event.MailMessage) {
	t.Helper()
	if _, reconsumeErr := event.Fold(state, 13, event.MailConsumed{IDs: []string{inbox.ID}, TurnID: "t4", Hop: 1}); reconsumeErr == nil {
		t.Fatal("Fold re-consumed a delivered message")
	}
	reset, err := event.Fold(state, 13, event.MailHumanReset{TurnID: "t4"})
	if err != nil {
		t.Fatalf("Fold human reset: %v", err)
	}
	if reset.Mail.Held || reset.Mail.HeldReason != "" || reset.Mail.AutoTurns != 0 {
		t.Fatalf("reset state = %#v", reset.Mail)
	}
}

func TestMailFoldConsumeSettleAndHold(t *testing.T) {
	t.Parallel()
	inbox := mailMessage("mail-"+strings.Repeat("6", 64), 1)
	sent := mailMessage("mail-"+strings.Repeat("7", 64), 1)
	sent.From, sent.To = sent.To, sent.From
	state := foldMailLifecycleState(t, inbox, sent)
	assertMailLifecycleFlags(t, state, inbox)
	assertMailHumanReset(t, state, inbox)
}

func TestMailFoldRejectsIllegal(t *testing.T) {
	t.Parallel()
	inbox := mailMessage("mail-"+strings.Repeat("8", 64), 1)
	sent := mailMessage("mail-"+strings.Repeat("9", 64), 1)
	sent.From, sent.To = sent.To, sent.From
	cases := []struct {
		name   string
		events []event.Event
	}{
		{"consume unknown", []event.Event{
			event.TurnStarted{TurnID: "t"}, event.MailConsumed{IDs: []string{inbox.ID}, TurnID: "t", Hop: 1},
		}},
		{"consume outside turn", []event.Event{
			event.MailReceived{Message: inbox}, event.MailConsumed{IDs: []string{inbox.ID}, TurnID: "t", Hop: 1},
		}},
		{"hop advanced outside turn", []event.Event{
			event.MailHopAdvanced{TurnID: "t", Hop: 2},
		}},
		{"settle conflict", []event.Event{
			event.MailOutgoing{Message: sent},
			event.MailSettled{ID: sent.ID, Outcome: "refused", Reason: "gone"},
			event.MailSettled{ID: sent.ID, Outcome: "delivered", RecipientTurnID: "rt"},
		}},
		{"received own outgoing", []event.Event{
			event.MailOutgoing{Message: sent}, event.MailReceived{Message: sent},
		}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if _, err := foldEvents(t, test.events...); err == nil {
				t.Fatal("Fold accepted an illegal mail transition")
			}
		})
	}
}
