package event_test

import (
	"encoding/json/jsontext"
	"math/rand/v2"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestAdvisorNoteRoundTripAndValidation(t *testing.T) {
	t.Parallel()
	note := event.AdvisorNote{Advisor: "advisor", Severity: "blocker", Note: "run the tests <now>", ReviewSeq: 42, Delivery: "steer"}
	encoded, err := event.Encode(note)
	if err != nil {
		t.Fatal(err)
	}
	want := `{"type":"advisor.note","advisor":"advisor","severity":"blocker","note":"run the tests <now>","review_seq":42,"delivery":"steer"}`
	if string(encoded) != want {
		t.Fatalf("encoded = %s", encoded)
	}
	decoded, err := event.Decode(encoded)
	if err != nil || !reflect.DeepEqual(decoded, note) {
		t.Fatalf("decoded = %#v, %v", decoded, err)
	}
	invalid := []event.AdvisorNote{
		{Severity: "nit", Note: "x", Delivery: "aside"},
		{Advisor: "a", Severity: "nit", Delivery: "aside"},
		{Advisor: "a", Severity: "warning", Note: "x", Delivery: "aside"},
		{Advisor: "a", Severity: "nit", Note: "x", Delivery: "later"},
		{Advisor: "a", Severity: "nit", Note: strings.Repeat("x", event.MaxInlineBytes+1), Delivery: "aside"},
	}
	for _, candidate := range invalid {
		if candidate.Validate() == nil {
			t.Fatalf("accepted %+v", candidate)
		}
	}
}

func TestAdvisorSessionNeedsParent(t *testing.T) {
	t.Parallel()
	created := event.SessionCreated{ID: "s", Cwd: "/w", Model: "m/x", Approval: "auto", Sandbox: "off", Runner: "local", Advisor: "advisor"}
	if created.Validate() == nil {
		t.Fatal("advisor session without parent accepted")
	}
	created.Parent = "p"
	encoded, err := event.Encode(created)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := event.Decode(encoded)
	if err != nil || !reflect.DeepEqual(decoded, created) {
		t.Fatalf("decoded = %#v, %v", decoded, err)
	}
}

func TestFoldRejectsDuplicateAdvisorNote(t *testing.T) {
	t.Parallel()
	state, err := event.Fold(event.State{}, 1, event.SessionCreated{ID: "s", Cwd: "/w", Model: "m/x", Approval: "auto", Sandbox: "off", Runner: "local"})
	if err != nil {
		t.Fatal(err)
	}
	note := event.AdvisorNote{Advisor: "advisor", Severity: "concern", Note: "check", ReviewSeq: 1, Delivery: "aside"}
	state, err = event.Fold(state, 2, note)
	if err != nil || state.Status != event.StatusIdle {
		t.Fatalf("idle note: %v %s", err, state.Status)
	}
	other := note
	other.ReviewSeq = 2
	if state, err = event.Fold(state, 3, other); err != nil {
		t.Fatalf("second review note: %v", err)
	}
	if _, err := event.Fold(state, 4, note); err == nil {
		t.Fatal("duplicate note accepted")
	}
}

func TestEncodeQuotesControlBytesAsJSON(t *testing.T) {
	t.Parallel()
	random := rand.New(rand.NewPCG(7, 11))
	source := rand.NewChaCha8([32]byte{7, 11})
	for range 200 {
		raw := make([]byte, random.IntN(40))
		if _, err := source.Read(raw); err != nil {
			t.Fatal(err)
		}
		text := string(raw)
		encoded, err := event.Encode(event.TurnCompleted{TurnID: "t", Status: "failed", Error: "e" + text})
		if err != nil {
			t.Fatalf("encode %q: %v", text, err)
		}
		if !jsontext.Value(encoded).IsValid() {
			t.Fatalf("invalid JSON for %q: %s", text, encoded)
		}
		decoded, err := event.Decode(encoded)
		if err != nil {
			t.Fatal(err)
		}
		want := "e" + strings.ToValidUTF8(text, "\uFFFD")
		if got := decoded.(event.TurnCompleted).Error; got != want {
			t.Fatalf("decoded %q, want %q", got, want)
		}
	}
}
