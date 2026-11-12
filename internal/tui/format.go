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
