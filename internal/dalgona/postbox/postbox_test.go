package postbox

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"fmt"
	"path/filepath"
	"sort"
	"strings"
	"testing"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type postFixture struct {
	host *agent.Host
	svc  *Service
	a    *agent.Thread
	b    *agent.Thread
	c    *agent.Thread
	idA  string
	idB  string
	idC  string
}

func testFixture(t *testing.T) *postFixture {
	t.Helper()
	home := t.TempDir()
	store, err := journal.Open(home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	cfg := agent.HostConfig{
		Journal: store,
		Config: config.Config{
			Home: home, Cwd: home, Model: "openai/gpt-test",
			Approval: config.ApprovalAuto, Sandbox: config.SandboxOff, Product: config.ProductDalgona,
		},
		Providers: func(agent.RetryFunc) (*model.Registry, error) { return model.NewRegistry(), nil },
		Tools:     func(context.Context, string, string) ([]tool.Tool, error) { return nil, nil },
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(home, id))
		},
	}
	host, err := agent.NewHost(cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = host.Close() })
	ctx := context.Background()
	fx := &postFixture{host: host}
	for _, slot := range []*struct {
		thread **agent.Thread
		id     *string
	}{{&fx.a, &fx.idA}, {&fx.b, &fx.idB}, {&fx.c, &fx.idC}} {
		started, startErr := host.Start(ctx, agent.ThreadSpec{})
		if startErr != nil {
			t.Fatal(startErr)
		}
		if appendErr := started.Append(ctx, event.TurnStarted{TurnID: "turn-1"}); appendErr != nil {
			t.Fatal(appendErr)
		}
		_, state, recordsErr := started.Records()
		if recordsErr != nil {
			t.Fatal(recordsErr)
		}
		*slot.thread = started
		*slot.id = state.SessionID
	}
	svc, err := New(ctx, Options{Host: host, Sessions: true, UserID: "test"})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = svc.Close(context.Background()) })
	fx.svc = svc
	return fx
}

func bindSender(t *testing.T, svc *Service, threadID string) *Sender {
	t.Helper()
	sender, err := svc.Bind(context.Background(), threadID, "turn-1")
	if err != nil {
		t.Fatalf("bind %s: %v", threadID, err)
	}
	return sender
}

func (fx *postFixture) newThread(t *testing.T) string {
	t.Helper()
	ctx := context.Background()
	created, createErr := fx.host.Start(ctx, agent.ThreadSpec{})
	if createErr != nil {
		t.Fatal(createErr)
	}
	if appendErr := created.Append(ctx, event.TurnStarted{TurnID: "turn-1"}); appendErr != nil {
		t.Fatal(appendErr)
	}
	_, state, recordsErr := created.Records()
	if recordsErr != nil {
		t.Fatal(recordsErr)
	}
	return state.SessionID
}

func mustPost(t *testing.T, sender *Sender, opID, to, text string) Receipt {
	t.Helper()
	receipt, err := sender.Post(context.Background(), Request{OperationID: opID, To: to, Text: text})
	if err != nil {
		t.Fatalf("post to %s: %v", to, err)
	}
	return receipt
}

func TestPostAllSessionPairs(t *testing.T) {
	t.Parallel()
	fx := testFixture(t)
	out := mustPost(t, bindSender(t, fx.svc, fx.idA), "call-1", fx.idB, "a to b")
	back := mustPost(t, bindSender(t, fx.svc, fx.idB), "call-1", fx.idA, "b to a")
	if out.PairSeq != 1 || back.PairSeq != 1 {
		t.Fatalf("pair seqs = %d, %d; independent pairs must each start at 1", out.PairSeq, back.PairSeq)
	}
	if out.Kind != ReceiptBuffered || out.Delivery != "queued" {
		t.Fatalf("receipt = %+v, want buffered/queued", out)
	}
	snap, err := fx.b.MailSnapshot(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Pending) != 1 || snap.Pending[0].Text != "a to b" {
		t.Fatalf("recipient pending = %+v", snap.Pending)
	}
}

func TestPostHostStampsSender(t *testing.T) {
	t.Parallel()
	fx := testFixture(t)
	receipt := mustPost(t, bindSender(t, fx.svc, fx.idA), "call-9", fx.idB, "stamped")
	compact, err := json.Marshal([3]string{fx.idA, "turn-1", "call-9"})
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(compact)
	if want := "mail-" + hex.EncodeToString(sum[:]); receipt.MessageID != want {
		t.Fatalf("message id = %s, want %s", receipt.MessageID, want)
	}
	snap, err := fx.b.MailSnapshot(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Pending) != 1 {
		t.Fatalf("pending = %d, want 1", len(snap.Pending))
	}
	got := snap.Pending[0]
	if got.From.SessionID != fx.idA || got.From.AgentID != "main" {
		t.Fatalf("from = %+v, want session %s agent main", got.From, fx.idA)
	}
	if got.To.SessionID != fx.idB || got.To.AgentID != "main" {
		t.Fatalf("to = %+v, want session %s agent main", got.To, fx.idB)
	}
}

func TestPostPairOrder(t *testing.T) {
	t.Parallel()
	fx := testFixture(t)
	sender := bindSender(t, fx.svc, fx.idA)
	for i, op := range []string{"c1", "c2", "c3"} {
		receipt := mustPost(t, sender, op, fx.idB, "msg")
		if receipt.PairSeq != uint64(i+1) {
			t.Fatalf("pair seq = %d, want %d", receipt.PairSeq, i+1)
		}
	}
	snap, err := fx.b.MailSnapshot(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Pending) != 3 {
		t.Fatalf("pending = %d, want 3", len(snap.Pending))
	}
	seqs := make([]uint64, 0, 3)
	for _, message := range snap.Pending {
		seqs = append(seqs, message.PairSeq)
	}
	sort.Slice(seqs, func(i, j int) bool { return seqs[i] < seqs[j] })
	for i, seq := range seqs {
		if seq != uint64(i+1) {
			t.Fatalf("pair seqs = %v, want 1,2,3 in order", seqs)
		}
	}
}

func (fx *postFixture) assertPostRefusal(t *testing.T, sender *Sender, ctx context.Context, state, fragment string) {
	t.Helper()
	if err := fx.svc.Transition(ctx, fx.idB, state); err != nil {
		t.Fatal(err)
	}
	_, err := sender.Post(ctx, Request{OperationID: "op-" + state, To: fx.idB, Text: "hi"})
	if err == nil || !strings.Contains(err.Error(), fragment) || !strings.Contains(err.Error(), fx.idB) {
		t.Fatalf("state %s err = %v, want recipient refusal naming the address", state, err)
	}
	if err := fx.svc.Transition(ctx, fx.idB, StateIdle); err != nil {
		t.Fatal(err)
	}
}

func TestPostReceiptCrashWindows(t *testing.T) {
	t.Parallel()
	fx := testFixture(t)
	ctx := context.Background()
	sender := bindSender(t, fx.svc, fx.idA)
	for state, fragment := range map[string]string{
		StateCrashed:  "crashed",
		StateFinished: "is finished",
		StateFailed:   "is failed",
		StateArchived: "is archived",
	} {
		fx.assertPostRefusal(t, sender, ctx, state, fragment)
	}
	if _, err := sender.Post(ctx, Request{OperationID: "op-gone", To: "missing-thread", Text: "hi"}); err == nil ||
		err.Error() != `post: unknown recipient "missing-thread"` {
		t.Fatalf("unknown err = %v", err)
	}
	if err := fx.svc.Transition(ctx, fx.idB, "bogus"); err == nil ||
		err.Error() != `post: invalid endpoint state "bogus"` {
		t.Fatalf("state err = %v", err)
	}
}

func assertPostTextLimit(t *testing.T, sender *Sender, ctx context.Context, to string) {
	t.Helper()
	if _, err := sender.Post(ctx, Request{OperationID: "big", To: to, Text: strings.Repeat("x", 16385)}); err == nil ||
		err.Error() != "post: text exceeds 16384 bytes" {
		t.Fatalf("long text err = %v", err)
	}
}

func assertPostInvalidAddresses(t *testing.T, sender *Sender, ctx context.Context) {
	t.Helper()
	for _, to := range []string{"", "has space", "no/such/node!", "g-0/n1", "g-xyz/n1"} {
		if _, err := sender.Post(ctx, Request{OperationID: "op", To: to, Text: "hi"}); err == nil ||
			!strings.HasPrefix(err.Error(), "post: invalid address") {
			t.Fatalf("to %q err = %v, want invalid address", to, err)
		}
	}
	if _, err := sender.Post(ctx, Request{OperationID: "op-absent", To: "UPPER", Text: "hi"}); err == nil ||
		err.Error() != `post: unknown recipient "UPPER"` {
		t.Fatalf("absent id err = %v, want unknown recipient", err)
	}
}

func assertPostSelfShapes(t *testing.T, sender *Sender, ctx context.Context, self, peer string) {
	t.Helper()
	if _, err := sender.Post(ctx, Request{OperationID: "self", To: self, Text: "hi"}); err == nil ||
		err.Error() != "post: cannot send to self" {
		t.Fatalf("self err = %v", err)
	}
	if _, err := sender.Post(ctx, Request{OperationID: "self-main", To: "main", Text: "hi"}); err == nil ||
		err.Error() != "post: cannot send to self" {
		t.Fatalf("self main err = %v", err)
	}
	if _, err := sender.Post(ctx, Request{OperationID: "blank", To: peer, Text: "   \t\n "}); err == nil ||
		err.Error() != "post: text is empty" {
		t.Fatalf("blank text err = %v", err)
	}
}

func postToRecipient(t *testing.T, sender *Sender, ctx context.Context, rid string, sent int) int {
	t.Helper()
	for range 3 {
		sent++
		op := fmt.Sprintf("agg-%d", sent)
		if sent <= 20 {
			mustPost(t, sender, op, rid, "fill")
			continue
		}
		_, err := sender.Post(ctx, Request{OperationID: op, To: rid, Text: "fill"})
		if err == nil || err.Error() != "post: sender rate limit is 20 messages per 60 seconds" {
			t.Fatalf("21st send err = %v, want sender rate (outbox must not trip on settled mail)", err)
		}
	}
	return sent
}

func assertPostSenderAggregate(t *testing.T, fx *postFixture, sender *Sender, ctx context.Context) {
	t.Helper()
	recipients := make([]string, 0, 7)
	for range 7 {
		recipients = append(recipients, fx.newThread(t))
	}
	sent := 0
	for _, rid := range recipients {
		sent = postToRecipient(t, sender, ctx, rid, sent)
	}
}

func postToInbox(t *testing.T, peer *Sender, ctx context.Context, to string, filled int) int {
	t.Helper()
	for range 3 {
		filled++
		op := fmt.Sprintf("inbox-%d", filled)
		if filled <= 20 {
			mustPost(t, peer, op, to, "fill")
			continue
		}
		_, err := peer.Post(ctx, Request{OperationID: op, To: to, Text: "fill"})
		if err == nil || err.Error() != "post: recipient mailbox is full (20)" {
			t.Fatalf("full inbox err = %v", err)
		}
	}
	return filled
}

func assertPostInboxFull(t *testing.T, fx *postFixture, ctx context.Context) {
	t.Helper()
	filled := 0
	for range 7 {
		peer := bindSender(t, fx.svc, fx.newThread(t))
		filled = postToInbox(t, peer, ctx, fx.idB, filled)
	}
	snap, err := fx.b.MailSnapshot(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snap.Pending) != 20 {
		t.Fatalf("pending = %d, want 20", len(snap.Pending))
	}
	for _, disposition := range snap.Dispositions {
		if disposition.Outcome != "pending" {
			t.Fatalf("disposition outcome = %q, want pending", disposition.Outcome)
		}
	}
}

func TestPostBounds(t *testing.T) {
	t.Parallel()
	fx := testFixture(t)
	ctx := context.Background()
	sender := bindSender(t, fx.svc, fx.idA)
	assertPostTextLimit(t, sender, ctx, fx.idB)
	assertPostInvalidAddresses(t, sender, ctx)
	assertPostSelfShapes(t, sender, ctx, fx.idA, fx.idB)
	assertPostSenderAggregate(t, fx, sender, ctx)
	assertPostInboxFull(t, fx, ctx)
}

func TestPostRejectionSettlesRefused(t *testing.T) {
	t.Parallel()
	fx := testFixture(t)
	ctx := context.Background()
	var peer *Sender
	for filled := range 20 {
		if filled%3 == 0 {
			peer = bindSender(t, fx.svc, fx.newThread(t))
		}
		mustPost(t, peer, fmt.Sprintf("fill-%d", filled), fx.idB, "fill")
	}
	_, err := peer.Post(ctx, Request{OperationID: "overflow", To: fx.idB, Text: "fill"})
	if err == nil || err.Error() != "post: recipient mailbox is full (20)" {
		t.Fatalf("overflow post err = %v", err)
	}
	if refused := countSettled(t, peer, event.MailOutcomeRefused); refused != 1 {
		t.Fatalf("refused settlements = %d, want 1", refused)
	}
	_, err = peer.Post(ctx, Request{OperationID: "overflow", To: fx.idB, Text: "fill"})
	if err == nil || !strings.Contains(err.Error(), "post: recipient refused message") {
		t.Fatalf("refused retry err = %v", err)
	}
}

func countSettled(t *testing.T, sender *Sender, outcome string) int {
	t.Helper()
	records, _, err := sender.thread.Records()
	if err != nil {
		t.Fatalf("sender records: %v", err)
	}
	count := 0
	for _, record := range records {
		settled, ok := record.Event.(event.MailSettled)
		if ok && settled.Outcome == outcome {
			count++
		}
	}
	return count
}

func TestPostArgShapes(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("t", 193)
	valid := `{"to":"main","text":"hi"}`
	if _, err := parsePostArgs(jsonValue(t, valid)); err != nil {
		t.Fatalf("valid args: %v", err)
	}
	cases := map[string]string{
		"unknown field":   `{"to":"main","text":"hi","extra":1}`,
		"missing text":    `{"to":"main"}`,
		"missing to":      `{"text":"hi"}`,
		"non-string text": `{"to":"main","text":["x"]}`,
		"bad reply_to":    `{"to":"main","text":"hi","reply_to":"nope"}`,
		"empty reply_to":  `{"to":"main","text":"hi","reply_to":""}`,
		"long to":         `{"to":"` + long + `","text":"hi"}`,
		"empty text":      `{"to":"main","text":""}`,
		"array":           `[]`,
		"string":          `"hi"`,
		"duplicate":       `{"to":"a","to":"b","text":"hi"}`,
	}
	for name, raw := range cases {
		args, err := parsePostArgs(jsonValue(t, raw))
		if err == nil {
			t.Fatalf("%s: unexpectedly accepted %+v", name, args)
		}
		if name == "unknown field" {
			if err.Error() != `post: unknown field "extra"` {
				t.Fatalf("%s err = %v", name, err)
			}
			continue
		}
		if err.Error() != "post: invalid arguments" {
			t.Fatalf("%s err = %v, want invalid arguments", name, err)
		}
	}
}

func jsonValue(t *testing.T, raw string) jsontext.Value {
	t.Helper()
	return jsontext.Value(raw)
}
