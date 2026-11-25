//go:build gui

package decision

import (
	"slices"
	"strconv"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	commandGrantNote     = "Accept for thread also accepts later commands in this thread that start with the same first two words and match this command's sandbox escalation. The grant stays in the thread journal across restarts. A compound command creates no grant."
	fileGrantNote        = "Accept for thread also accepts later edits in this thread made with the same tool. The grant stays in the thread journal across restarts."
	trustNote            = "Trust workspace starts these servers and saves the workspace as trusted. Decline keeps them stopped until the dalgona host restarts. Not now keeps them stopped and asks again on the next turn."
	handleNote           = "Access applies only to this workspace handle, until it expires or is revoked."
	sandboxHoldNote      = "The choice holds for this thread. Cancel declines this command, and the next command asks again."
	sandboxAcceptNote    = "Every command in this thread then asks for your approval first, and thread grants do not apply."
	sandboxDangerousNote = "Commands in this thread then run without confinement under the thread's approval mode."
	escalationNote       = "Escalated: this command runs outside the harness sandbox."
)

var rightNotes = map[string]string{
	"read":     "Read: raw file contents under the root become readable from this window.",
	"write":    "Write: files under the root can be created, changed, renamed and removed from this window.",
	"terminal": "Terminal: commands can run in an interactive terminal on the runner.",
}

func rightsNotes(rights []string) []string {
	notes := make([]string, 0, len(rights))
	for _, r := range rights {
		if note, ok := rightNotes[r]; ok && !slices.Contains(notes, note) {
			notes = append(notes, note)
		}
	}
	return notes
}

type outcome struct {
	here, elsewhere string
}

var questionOutcomes = map[string]outcome{
	"answered":    {here: "Question resolved as answered", elsewhere: "Question answered in another window"},
	"cancelled":   {here: "Question resolved as dismissed", elsewhere: "Question dismissed in another window"},
	"interrupted": {here: "Question closed: the turn was interrupted", elsewhere: "Question closed: the turn was interrupted"},
	"declined":    {here: "Question closed: the agent continues without an answer", elsewhere: "Question closed: the agent continues without an answer"},
	"timedOut":    {here: "Question timed out", elsewhere: "Question timed out"},
}

var approvalOutcomes = map[string]outcome{
	"answered":    {here: "Request resolved as accepted", elsewhere: "Request accepted in another window"},
	"declined":    {here: "Request resolved as declined", elsewhere: "Request declined"},
	"cancelled":   {here: "Request resolved as cancelled", elsewhere: "Request cancelled in another window"},
	"interrupted": {here: "Request closed: the turn was interrupted", elsewhere: "Request closed: the turn was interrupted"},
	"timedOut":    {here: "Request timed out", elsewhere: "Request timed out"},
}

func resolutionText(r state.ResolvedRequest, endpoint string) string {
	table, subject := approvalOutcomes, "Request"
	if r.Method == methodQuestion {
		table, subject = questionOutcomes, "Question"
	}
	o, ok := table[r.Resolution]
	if !ok {
		return subject + " resolved: " + r.Resolution + " on " + endpoint
	}
	if r.AnsweredHere {
		return o.here + " on " + endpoint
	}
	return o.elsewhere + " on " + endpoint
}

func kickerLine(kicker string, total int, endpoint string) string {
	if total > 1 {
		kicker += " · 1 of " + strconv.Itoa(total)
	}
	return kicker + " · on " + endpoint
}
