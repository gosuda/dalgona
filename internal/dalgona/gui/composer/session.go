//go:build gui

package composer

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	interruptWindow = 2 * time.Second
	settleDelay     = 120 * time.Millisecond
	shellOutputCap  = "64 KiB"
)

type interruptGuard struct{ until time.Time }

func (g *interruptGuard) escape(now time.Time, rateLimited bool) bool {
	if rateLimited || now.Before(g.until) {
		g.until = time.Time{}
		return true
	}
	g.until = now.Add(interruptWindow)
	return false
}

func (g interruptGuard) armed(now time.Time) bool { return now.Before(g.until) }

type recall struct {
	browsing int
	saved    inputtext.Draft
}

func recallHistory(prompts, sent []string) []string {
	texts := make([]string, 0, len(prompts)+len(sent))
	for _, p := range prompts {
		if p != "" && (len(texts) == 0 || texts[len(texts)-1] != p) {
			texts = append(texts, p)
		}
	}
	for _, s := range sent {
		if s != "" && !slices.Contains(texts, s) {
			texts = append(texts, s)
		}
	}
	return texts
}

func (r *recall) idle() bool { return r.browsing < 0 }

func (r *recall) older(history []string, current inputtext.Draft) (inputtext.Draft, bool) {
	at := r.browsing - 1
	if r.idle() {
		at = len(history) - 1
	}
	if at < 0 {
		return inputtext.Draft{}, false
	}
	if r.idle() {
		r.saved = current
	}
	r.browsing = at
	return inputtext.Draft{Text: history[at]}, true
}

func (r *recall) newer(history []string) (inputtext.Draft, bool) {
	if r.idle() {
		return inputtext.Draft{}, false
	}
	r.browsing++
	if r.browsing < len(history) {
		return inputtext.Draft{Text: history[r.browsing]}, true
	}
	saved := r.saved
	r.reset()
	return saved, true
}

func (r *recall) reset() { r.browsing, r.saved = -1, inputtext.Draft{} }

func shellChipName(n, exit, lines int, truncated bool) string {
	unit := "lines"
	if lines == 1 {
		unit = "line"
	}
	name := fmt.Sprintf("[Shell #%d exit %d, %d %s]", n, exit, lines, unit)
	if truncated {
		name += " · output truncated at " + shellOutputCap
	}
	return name
}

func shellOutput(raw string) (string, int) {
	output := strings.TrimRight(strings.ToValidUTF8(raw, "\uFFFD"), "\n")
	return output, lineCount(output)
}

func shellText(command string, exit int, output string, truncated bool) string {
	longest, run := 0, 0
	for i := 0; i < len(output); i++ {
		if output[i] != '`' {
			run = 0
			continue
		}
		run++
		longest = max(longest, run)
	}
	fence := strings.Repeat("`", max(3, longest+1))
	note := ""
	if truncated {
		note = ", truncated at " + shellOutputCap
	}
	return fmt.Sprintf("Output of `%s` (exit %d%s):\n%s\n%s\n%s", command, exit, note, fence, output, fence)
}
