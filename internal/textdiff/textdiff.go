package textdiff

import (
	"fmt"
	"strings"

	udiff "github.com/aymanbagabas/go-udiff"
)

const Limit = 1 << 20

type Stats struct {
	Added   int
	Removed int
}

func Unified(oldName, newName string, pre, post []byte, context int) (string, Stats, error) {
	text, err := udiff.ToUnified(oldName, newName, string(pre), udiff.Lines(string(pre), string(post)), context)
	if err != nil {
		return "", Stats{}, fmt.Errorf("diff %s: %w", newName, err)
	}
	var stats Stats
	lines := strings.Split(text, "\n")
	if len(lines) > 2 {
		for _, line := range lines[2:] {
			switch {
			case strings.HasPrefix(line, "+"):
				stats.Added++
			case strings.HasPrefix(line, "-"):
				stats.Removed++
			}
		}
	}
	return text, stats, nil
}
