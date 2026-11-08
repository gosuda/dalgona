package advisor

import (
	_ "embed"
	"strconv"
	"strings"
)

//go:embed prompts/system.md
var systemTemplate string

//go:embed prompts/advise.md
var AdviseDescription string

func System(maxNotes int, instructions, watchdog string) string {
	parts := make([]string, 0, 3)
	parts = append(parts, strings.Replace(strings.TrimSpace(systemTemplate), "{{max_notes}}", strconv.Itoa(maxNotes), 1))
	if instructions = strings.TrimSpace(instructions); instructions != "" {
		parts = append(parts, "<specialization>\n"+instructions+"\n</specialization>")
	}
	if watchdog != "" {
		parts = append(parts, watchdog)
	}
	return strings.Join(parts, "\n\n")
}
