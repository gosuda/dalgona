package tui

import (
	"fmt"
	"time"
)

func formatElapsed(d time.Duration) string {
	switch {
	case d < 0:
		return "0 ms"
	case d < time.Second:
		return fmt.Sprintf("%d ms", d.Milliseconds())
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d/time.Second))
	case d < time.Hour:
		return fmt.Sprintf("%dm %02ds", int(d/time.Minute), int(d%time.Minute/time.Second))
	}
	hours := int(d / time.Hour)
	if hours > 99 {
		return "99h 59m"
	}
	return fmt.Sprintf("%dh %02dm", hours, int(d%time.Hour/time.Minute))
}

const resumeDateCells = 7

func formatResumeDate(t time.Time, now time.Time) string {
	if t.IsZero() {
		return ""
	}
	local := t.In(now.Location())
	if local.Year() < 1 || local.Year() > 9999 {
		return ""
	}
	if local.Year() == now.Year() {
		return local.Format("01-02")
	}
	return local.Format("2006-01")
}
