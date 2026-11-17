package daltoki

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

type fakeIntakeStore struct {
	mu        sync.Mutex
	envelopes []Envelope
	failAfter int
}

func (f *fakeIntakeStore) Admit(ctx context.Context, envelope Envelope) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.failAfter >= 0 && len(f.envelopes) >= f.failAfter {
		return false, fmt.Errorf("daltoki: intake store unavailable")
	}
	f.envelopes = append(f.envelopes, envelope)
	return true, nil
}

func (f *fakeIntakeStore) admitted() []Envelope {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]Envelope(nil), f.envelopes...)
}

type relayHarness struct {
	server *httptest.Server
	pulls  int
	acks   []string
	mu     sync.Mutex
}

func newRelayHarness(t *testing.T, items []Envelope) *relayHarness {
	t.Helper()
	h := &relayHarness{}
	h.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/daltoki/relay/pull":
			h.mu.Lock()
			h.pulls++
			h.mu.Unlock()
			body, _ := json.Marshal(RelayPage{Items: items, Next: lastDelivery(items)})
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write(body)
		case "/daltoki/relay/ack":
			var decoded struct {
				Through string `json:"through"`
			}
			if err := json.Unmarshal(readBody(t, r), &decoded); err != nil {
				t.Errorf("ack decode: %v", err)
			}
			h.mu.Lock()
			h.acks = append(h.acks, decoded.Through)
			h.mu.Unlock()
			w.WriteHeader(http.StatusNoContent)
		default:
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	t.Cleanup(h.server.Close)
	return h
}

func lastDelivery(items []Envelope) string {
	if len(items) == 0 {
		return ""
	}
	return items[len(items)-1].DeliveryID
}

func signedEnvelope(secret, id, event string, body []byte, at time.Time) Envelope {
	return Envelope{
		DeliveryID: id,
		Event:      event,
		ReceivedAt: at,
		BodySHA256: BodyDigest(body),
		Signature:  signBody(secret, body)[len("sha256="):],
		Body:       body,
	}
}

func testRelayIntake(t *testing.T, server *httptest.Server, store IntakeStore, secret string) *RelayIntake {
	t.Helper()
	client, err := NewRelayClient(server.URL, "relay-token", server.Client())
	if err != nil {
		t.Fatalf("NewRelayClient: %v", err)
	}
	return NewRelayIntake(client, secret, store, time.Now)
}

func TestRelayAckAfterDurability(t *testing.T) {
	secret := "relay-secret"
	now := time.Now()
	items := []Envelope{
		signedEnvelope(secret, "delivery-1", "issues", []byte(`{"action":"opened"}`), now),
		signedEnvelope(secret, "delivery-2", "issue_comment", []byte(`{"action":"created"}`), now),
	}
	harness := newRelayHarness(t, items)
	store := &fakeIntakeStore{failAfter: -1}
	intake := testRelayIntake(t, harness.server, store, secret)

	next, err := intake.PollOnce(context.Background(), "")
	if err != nil {
		t.Fatalf("PollOnce: %v", err)
	}
	if next != "delivery-2" {
		t.Fatalf("next = %q", next)
	}
	if got := len(store.admitted()); got != 2 {
		t.Fatalf("admitted = %d", got)
	}
	harness.mu.Lock()
	acks := append([]string(nil), harness.acks...)
	harness.mu.Unlock()
	if len(acks) != 1 || acks[0] != "delivery-2" {
		t.Fatalf("acks = %v", acks)
	}
}

func TestRelayAckHoldsUndurableTail(t *testing.T) {
	secret := "relay-secret"
	now := time.Now()
	items := []Envelope{
		signedEnvelope(secret, "delivery-1", "issues", []byte(`{"action":"opened"}`), now),
		signedEnvelope(secret, "delivery-2", "issue_comment", []byte(`{"action":"created"}`), now),
		signedEnvelope(secret, "delivery-3", "issues", []byte(`{"action":"closed"}`), now),
	}
	harness := newRelayHarness(t, items)
	store := &fakeIntakeStore{failAfter: 1}
	intake := testRelayIntake(t, harness.server, store, secret)

	next, err := intake.PollOnce(context.Background(), "")
	if err == nil {
		t.Fatal("expected store error")
	}
	if next != "delivery-1" {
		t.Fatalf("next = %q", next)
	}
	harness.mu.Lock()
	acks := append([]string(nil), harness.acks...)
	harness.mu.Unlock()
	if len(acks) != 1 || acks[0] != "delivery-1" {
		t.Fatalf("acks = %v", acks)
	}
	if got := len(store.admitted()); got != 1 {
		t.Fatalf("admitted = %d", got)
	}
}

func TestRelayRejectsForgedEnvelopeWithoutAck(t *testing.T) {
	secret := "relay-secret"
	now := time.Now()
	forged := signedEnvelope("attacker", "delivery-9", "issues", []byte(`{"action":"opened"}`), now)
	harness := newRelayHarness(t, []Envelope{forged})
	store := &fakeIntakeStore{failAfter: -1}
	intake := testRelayIntake(t, harness.server, store, secret)

	if _, err := intake.PollOnce(context.Background(), ""); err != nil {
		t.Fatalf("PollOnce: %v", err)
	}
	if got := len(store.admitted()); got != 0 {
		t.Fatalf("forged envelope admitted: %d", got)
	}
	harness.mu.Lock()
	acks := append([]string(nil), harness.acks...)
	harness.mu.Unlock()
	if len(acks) != 1 || acks[0] != "delivery-9" {
		t.Fatalf("acks = %v", acks)
	}
}

func testPollClient(t *testing.T, server *httptest.Server) *github.Client {
	t.Helper()
	client, err := github.NewClient(github.ClientConfig{
		BaseURL:      server.URL,
		Repositories: []github.Repository{{Owner: "octo", Name: "repo"}},
		Policy:       "bot",
	}, stubCredential{}, nil)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	return client
}

func readBody(t *testing.T, r *http.Request) []byte {
	t.Helper()
	raw, err := io.ReadAll(r.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	return raw
}
