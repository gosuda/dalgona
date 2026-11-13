package agent

import (
	"slices"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func stampTimes(events []event.Event, now time.Time) []event.Event {
	out := slices.Clone(events)
	for i, item := range out {
		out[i] = stampTime(item, now)
	}
	return out
}

func stampTime(item event.Event, now time.Time) event.Event {
	switch value := item.(type) {
	case event.TurnStarted:
		value.StartedAt = orNow(value.StartedAt, now)
		return value
	case event.TurnCompleted:
		value.CompletedAt = orNow(value.CompletedAt, now)
		return value
	case event.ToolStarted:
		value.StartedAt = orNow(value.StartedAt, now)
		return value
	case event.ToolCompleted:
		value.CompletedAt = orNow(value.CompletedAt, now)
		return value
	default:
		return item
	}
}

func orNow(at, now time.Time) time.Time {
	if at.IsZero() {
		return now
	}
	return at
}
