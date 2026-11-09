package agent

import (
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestSettleTurnLogsDroppedContinuation(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	if _, err := ask(thread, func(a *actor) bool {
		a.damage = errors.New("agent: test damage")
		a.records = append(a.records, event.Seqd{Seq: a.head() + 1, Event: event.UserMessage{}})
		return true
	}); err != nil {
		t.Fatalf("agent: damage thread: %v", err)
	}
	if _, err := ask(thread, func(a *actor) bool {
		return a.settleTurn("turn-test", outcome{status: statusCompleted}, 0)
	}); err != nil {
		t.Fatalf("agent: settleTurn: %v", err)
	}
	logs := h.logs.String()
	if !strings.Contains(logs, "journal turn.completed") {
		t.Fatalf("agent: missing turn.completed error log: %s", logs)
	}
	if !strings.Contains(logs, "journal turn.started") {
		t.Fatalf("agent: dropped continuation turn.started without a log: %s", logs)
	}
}
