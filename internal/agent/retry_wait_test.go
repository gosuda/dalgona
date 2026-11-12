package agent

import (
	"net/http"
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/model"
)

const rateLimitBody = `{"error":{"type":"rate_limit_error"}}`

func awaitUpdate(t *testing.T, updates <-chan Update, match func(Update) bool) Update {
	t.Helper()
	timeout := time.After(10 * time.Second)
	for {
		select {
		case update := <-updates:
			if match(update) {
				return update
			}
		case <-timeout:
			t.Fatal("no matching update within 10 seconds")
		}
	}
}

func TestRetryWaitReachesAttachedClient(t *testing.T) {
	h := newHarness(t)
	h.model.push(response{status: http.StatusTooManyRequests, retryAfter: "90", body: rateLimitBody}, text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "wait for me")
	var waits []time.Duration
	for {
		update := awaitUpdate(t, updates, func(u Update) bool { return u.Kind == UpdateRetryWait })
		waits = append(waits, update.Wait)
		if update.Wait == 0 {
			break
		}
	}
	if len(waits) < 2 || waits[0] != 90*time.Second {
		t.Fatalf("waits = %v, want a first wait of 90s followed by 0", waits)
	}
	h.waitIdle(thread)
	if !slices.Contains(h.types(thread.ID()), "turn.completed") {
		t.Fatalf("events = %v, want the turn to complete", h.types(thread.ID()))
	}
}

func TestRetryWaitStopsWithoutSubscriber(t *testing.T) {
	h := newHarness(t)
	h.model.push(response{status: http.StatusTooManyRequests, retryAfter: "90", body: rateLimitBody})
	thread := h.start()
	h.submit(thread, "wait for me")
	h.waitIdle(thread)
	const want = "rate limited for 90 s, over the 60 s wait limit"
	if got := lastTurn(h.events(thread.ID())).Error; got != want {
		t.Fatalf("turn error = %q, want %q", got, want)
	}
}

func TestInterruptEndsRetryWait(t *testing.T) {
	h := newHarness(t, func(h *harness) { h.sleep = model.DefaultRetryOptions().Sleep })
	h.model.push(response{status: http.StatusTooManyRequests, retryAfter: "90", body: rateLimitBody}, text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "wait for me")
	awaitUpdate(t, updates, func(u Update) bool { return u.Kind == UpdateRetryWait && u.Wait > 0 })
	started := time.Now()
	thread.Interrupt()
	last := awaitUpdate(t, updates, func(u Update) bool { return u.Kind == UpdateRetryWait && u.Wait == 0 })
	h.waitIdle(thread)
	if elapsed := time.Since(started); elapsed > 2*time.Second {
		t.Fatalf("interrupt took %v to end the wait, want under 2s", elapsed)
	}
	if got := lastTurn(h.events(thread.ID())).Status; got != statusInterrupted {
		t.Fatalf("turn status = %q, want %q", got, statusInterrupted)
	}
	if got := h.model.count(); got != 1 {
		t.Fatalf("model requests = %d, want 1 before the interrupt", got)
	}
	if last.Wait != 0 {
		t.Fatalf("last wait = %v, want 0", last.Wait)
	}
}
