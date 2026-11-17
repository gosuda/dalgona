package agent

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
)

func postTestThreads(t *testing.T, h *harness) (*Thread, *Thread) {
	t.Helper()
	sender := h.start()
	recipient := h.start()
	ctx := t.Context()
	for _, thread := range []*Thread{sender, recipient} {
		if err := thread.Append(ctx, event.TurnStarted{TurnID: "turn-1"}); err != nil {
			t.Fatal(err)
		}
	}
	return sender, recipient
}

func recipientAddress(t *testing.T, recipient *Thread) event.MailAddress {
	t.Helper()
	_, state, err := recipient.Records()
	if err != nil {
		t.Fatal(err)
	}
	return event.MailAddress{SessionID: state.SessionID, AgentID: "main"}
}

func assertJournalMasked(t *testing.T, records []event.Seqd, secret, owner string) {
	t.Helper()
	for _, record := range records {
		encoded, encodeErr := event.Encode(record.Event)
		if encodeErr != nil {
			t.Fatal(encodeErr)
		}
		if strings.Contains(string(encoded), secret) {
			t.Fatalf("%s for %s carry the raw secret", owner, record.Event.Type())
		}
	}
}

func TestMailMaskBeforePersistence(t *testing.T) {
	secret := "token-abcdefgh1234"
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) { h.cfg.Auth = store })
	sender, recipient := postTestThreads(t, h)
	ctx := t.Context()
	message, err := sender.MailOut(ctx, MailOutRequest{
		OriginTurnID: "turn-1", OperationID: "call-1",
		Text: "deploy with " + secret + " now",
		To:   recipientAddress(t, recipient),
	})
	if err != nil {
		t.Fatalf("MailOut: %v", err)
	}
	if strings.Contains(message.Text, secret) {
		t.Fatal("returned message carries the raw secret")
	}
	if recvErr := recipient.MailReceive(ctx, message); recvErr != nil {
		t.Fatalf("MailReceive: %v", recvErr)
	}
	records, _, err := recipient.Records()
	if err != nil {
		t.Fatal(err)
	}
	assertJournalMasked(t, records, secret, "journal bytes")
	senderRecords, _, err := sender.Records()
	if err != nil {
		t.Fatal(err)
	}
	assertJournalMasked(t, senderRecords, secret, "sender journal bytes")
}

func mustMailOut(t *testing.T, sender *Thread, req MailOutRequest) event.MailMessage {
	t.Helper()
	message, err := sender.MailOut(t.Context(), req)
	if err != nil {
		t.Fatalf("MailOut: %v", err)
	}
	return message
}

func expectMailOutErr(t *testing.T, sender *Thread, req MailOutRequest, want, label string) {
	t.Helper()
	_, err := sender.MailOut(t.Context(), req)
	if err == nil || err.Error() != want {
		t.Fatalf("%s err = %v, want %s", label, err, strings.TrimPrefix(want, "post: "))
	}
}

func TestMailIdentityConflict(t *testing.T) {
	sender, to := mailConflictFixture(t)
	first := mustMailOut(t, sender, MailOutRequest{
		OriginTurnID: "turn-1", OperationID: "call-1", Text: "first body", To: to,
	})
	retry := mustMailOut(t, sender, MailOutRequest{
		OriginTurnID: "turn-1", OperationID: "call-1", Text: "first body", To: to,
	})
	if retry.ID != first.ID || retry.PairSeq != first.PairSeq {
		t.Fatalf("retry = %+v, want identical to %+v", retry, first)
	}
	expectMailOutErr(t, sender, MailOutRequest{
		OriginTurnID: "turn-1", OperationID: "call-1", Text: "different body", To: to,
	}, "post: message id conflict", "changed payload")
	other := mustMailOut(t, sender, MailOutRequest{
		OriginTurnID: "turn-1", OperationID: "call-2", Text: "different body", To: to,
	})
	if other.ID == first.ID {
		t.Fatal("new operation must mint a new id")
	}
	if other.PairSeq != first.PairSeq+1 {
		t.Fatalf("pair seq = %d, want %d", other.PairSeq, first.PairSeq+1)
	}
	expectMailOutErr(t, sender, MailOutRequest{
		OriginTurnID: "turn-9", OperationID: "call-3", Text: "stray turn", To: to,
	}, "post: invalid originating operation", "foreign turn")
}

func mailConflictFixture(t *testing.T) (*Thread, event.MailAddress) {
	t.Helper()
	sender, recipient := postTestThreads(t, newHarness(t))
	return sender, recipientAddress(t, recipient)
}
