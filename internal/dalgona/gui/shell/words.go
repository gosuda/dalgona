//go:build gui

package shell

import (
	"fmt"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type Word uint8

const (
	Unknown Word = iota
	Working
	Waiting
	Done
	Failed
	Declined
	TimedOut
	Ready
	Stopped
	Offline
)

var wordText = [...]string{
	Working:  "Working",
	Waiting:  "Waiting",
	Done:     "Done",
	Failed:   "Failed",
	Declined: "Declined",
	TimedOut: "Timed out",
	Ready:    "Ready",
	Stopped:  "Stopped",
	Offline:  "Offline",
}

const (
	reasonApproval = "needs approval"
	reasonAnswer   = "needs an answer"
)

type Tag struct {
	Word    Word
	Text    string
	Reason  string
	Started time.Time
	Retry   time.Time
	Point   bool
}

func (t Tag) Name() string {
	if t.Word == Unknown {
		return t.Text
	}
	return wordText[t.Word]
}

func (t Tag) Label() string {
	if t.Reason == "" {
		return t.Name()
	}
	return t.Name() + " · " + t.Reason
}

var itemWords = map[string]Word{
	"inProgress":  Working,
	"completed":   Done,
	"failed":      Failed,
	"declined":    Declined,
	"timedOut":    TimedOut,
	"interrupted": Stopped,
}

func ItemTag(status string) Tag {
	if w, ok := itemWords[status]; ok {
		return Tag{Word: w}
	}
	return Tag{Text: status}
}

var turnWords = map[string]Word{
	"inProgress":  Working,
	"completed":   Done,
	"failed":      Failed,
	"interrupted": Stopped,
}

func requestReason(method string) string {
	if strings.HasSuffix(method, "requestUserInput") {
		return reasonAnswer
	}
	return reasonApproval
}

func pendingReason(tv *state.ThreadView) (string, bool) {
	for _, r := range tv.Requests {
		if !r.Answered {
			return requestReason(r.Method), true
		}
	}
	return "", false
}

func flagReason(flags []string) (string, bool) {
	for _, f := range flags {
		switch f {
		case "waitingOnApproval":
			return reasonApproval, true
		case "waitingOnUserInput":
			return reasonAnswer, true
		}
	}
	return "", false
}

func threadTag(tv *state.ThreadView, phase client.Phase) Tag {
	if phase == client.PhaseOffline || tv.Offline {
		return Tag{Word: Offline}
	}
	if reason, ok := pendingReason(tv); ok {
		return Tag{Word: Waiting, Reason: reason}
	}
	if len(tv.Turns) == 0 {
		return summaryTag(tv.Thread.Status)
	}
	last := tv.Turns[len(tv.Turns)-1]
	if tv.ActiveTurn != "" || last.Status == "inProgress" {
		return workingTag(tv, last)
	}
	if w, ok := turnWords[last.Status]; ok {
		return Tag{Word: w}
	}
	return Tag{Text: last.Status}
}

func workingTag(tv *state.ThreadView, last state.TurnView) Tag {
	if reason, ok := flagReason(tv.Thread.Status.ActiveFlags); ok {
		return Tag{Word: Waiting, Reason: reason}
	}
	t := Tag{Word: Working}
	if last.StartedAt > 0 {
		t.Started = time.Unix(last.StartedAt, 0)
	}
	if last.RetryWait > 0 {
		t.Retry = last.RetryWaitAt.Add(last.RetryWait)
	}
	return t
}

func summaryTag(status rpc.ThreadStatus) Tag {
	switch status.Type {
	case "active":
		if reason, ok := flagReason(status.ActiveFlags); ok {
			return Tag{Word: Waiting, Reason: reason}
		}
		return Tag{Word: Working}
	case "idle", "notLoaded", "":
		return Tag{Word: Ready}
	case "systemError":
		return Tag{Word: Failed}
	}
	return Tag{Text: status.Type}
}

func rowTag(t rpc.Thread, tv *state.ThreadView, phase client.Phase) Tag {
	if tv != nil && (len(tv.Turns) > 0 || len(tv.Requests) > 0) {
		tag := threadTag(tv, phase)
		tag.Started, tag.Retry = time.Time{}, time.Time{}
		return tag
	}
	return summaryTag(t.Status)
}

func formatDuration(d time.Duration) string {
	ms := d.Milliseconds()
	switch {
	case d < time.Second:
		return fmt.Sprintf("%d ms", ms)
	case d < time.Minute:
		return fmt.Sprintf("%ds", ms/1000)
	case d < time.Hour:
		return fmt.Sprintf("%dm %02ds", ms/60000, ms/1000%60)
	}
	return fmt.Sprintf("%dh %02dm", ms/3600000, ms/60000%60)
}

func formatRetry(d time.Duration) string {
	ms := max(0, d.Milliseconds())
	return fmt.Sprintf("rate limited · retrying in %dm %02ds · esc stops", ms/60000, ms/1000%60)
}

func threadTitle(t rpc.Thread) string {
	switch {
	case t.Name != "":
		return t.Name
	case t.Preview != "":
		return t.Preview
	}
	return "Untitled thread"
}

var phaseWords = [...]string{
	client.PhaseIdle:         "Idle",
	client.PhaseConnecting:   "Connecting",
	client.PhaseConnected:    "Connected",
	client.PhaseReconnecting: "Reconnecting",
	client.PhaseOffline:      "Offline",
	client.PhaseIncompatible: "Incompatible",
	client.PhaseFailed:       "Failed",
}

func phaseWord(p client.Phase) string {
	if int(p) < len(phaseWords) {
		return phaseWords[p]
	}
	return p.String()
}
