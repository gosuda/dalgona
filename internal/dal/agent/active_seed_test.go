package agent

import (
	"context"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func TestSeededSubscriptionPartitionsConcurrentDeltas(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	if err := thread.Append(t.Context(), event.TurnStarted{TurnID: "turn"}); err != nil {
		t.Fatal(err)
	}
	status, err := thread.Status()
	if err != nil {
		t.Fatal(err)
	}
	initial := strings.Repeat("a", 260<<10)
	publishSeedTestDelta(thread, initial)
	wantTail, published := seedTestTail(thread)
	sub, err := thread.SubscribeSeeded(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer thread.Unsubscribe(sub.Updates)
	defer sub.Release()
	if sub.Head != status.Head {
		t.Fatalf("subscription head = %d, want durable cut at %d", sub.Head, status.Head)
	}
	<-published
	publishSeedTestDelta(thread, "late")
	thread.publish(Update{Kind: UpdateWarning, Warning: "end"})
	var text strings.Builder
	for update, readErr := range sub.Seed {
		if readErr != nil {
			t.Fatal(readErr)
		}
		appendSeedTestDelta(t, &text, update)
	}
	takeSeedTestDeltas(t, sub.Updates, &text)
	want := initial + wantTail + "late"
	if text.String() != want {
		t.Fatalf("seed plus live stream has %d bytes, want %d bytes in order without duplicates", text.Len(), len(want))
	}
}

func publishSeedTestDelta(thread *Thread, text string) {
	thread.publish(Update{Kind: UpdateTextDelta, ItemID: "item", Delta: text})
}

func seedTestTail(thread *Thread) (string, <-chan struct{}) {
	tail := make([]string, 64)
	for i := range tail {
		tail[i] = strconv.Itoa(i) + ":"
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for _, text := range tail {
			publishSeedTestDelta(thread, text)
		}
	}()
	return strings.Join(tail, ""), done
}

func appendSeedTestDelta(t *testing.T, text *strings.Builder, update Update) {
	t.Helper()
	if update.Kind != UpdateTextDelta || update.ItemID != "item" {
		t.Fatalf("unexpected delta: kind=%d item=%q", update.Kind, update.ItemID)
	}
	text.WriteString(update.Delta)
}

func takeSeedTestDeltas(t *testing.T, updates <-chan Update, text *strings.Builder) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	for {
		select {
		case update := <-updates:
			if update.Kind == UpdateWarning && update.Warning == "end" {
				return
			}
			appendSeedTestDelta(t, text, update)
		case <-ctx.Done():
			t.Fatal("live subscriber did not observe stream end")
		}
	}
}

func TestSeededSubscriptionReportsLostDelivery(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	sub, err := thread.SubscribeSeeded(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	defer thread.Unsubscribe(sub.Updates)
	defer sub.Release()
	for range subscriberBuffer + 1 {
		publishSeedTestDelta(thread, "overflow")
	}
	if _, statusErr := thread.Status(); statusErr != nil {
		t.Fatal(statusErr)
	}
	if !sub.Overflowed() {
		t.Fatal("seeded subscription closed without reporting lost delivery")
	}
}
