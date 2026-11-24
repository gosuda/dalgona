//go:build gui

package shell

import (
	"strconv"
	"strings"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	keepOpenLabel        = "Keep open"
	closeAnywayLabel     = "Close anyway"
	closeExplanation     = "When no other client watches these threads, each approval is declined and each question ends with no answer. The daemon keeps running, but these requests do not stay open."
	windowExplanation    = "Closing this window discards its unsent drafts and queued follow-ups. Other windows keep their own."
	exitExplanation      = "Drafts that could not be saved and queued follow-ups do not survive closing dalgona."
	decisionsTitle       = "Close with pending decisions?"
	unsentTitle          = "Close with unsent text?"
	terminalsTitle       = "Close and detach the terminal sessions?"
	terminalsExplanation = "Closing this window detaches its terminal sessions. The processes keep running until they exit or their lease ends; Stop terminal closes a process explicitly."
)

type CloseScope struct {
	Elsewhere func(state.ThreadKey) bool
	Final     bool
	Terminals []string
}

type closeFlow struct {
	decisions []string
	unsent    []string
	terminals []string
	final     bool
	confirmed bool
}

func (c closeFlow) asking() bool { return len(c.decisions)+len(c.unsent)+len(c.terminals) > 0 }

func (s *Shell) Closing(v *state.View, scope CloseScope) bool {
	if s.close.confirmed {
		return false
	}
	flow := closeFlow{decisions: affectedDecisions(v, scope.Elsewhere), unsent: s.lostContent(v, scope.Final), terminals: scope.Terminals, final: scope.Final}
	if !flow.asking() {
		return false
	}
	s.close = flow
	s.focus.request(&s.keepOpen)
	return true
}

func decisionKind(method string) string {
	if strings.HasSuffix(method, "requestUserInput") {
		return "Question"
	}
	return "Approval"
}

func threadName(v *state.View, key state.ThreadKey) string {
	if tv, ok := v.Thread(key); ok && tv.Thread.ID != "" {
		return threadTitle(tv.Thread)
	}
	return key.Thread
}

func affectedDecisions(v *state.View, elsewhere func(state.ThreadKey) bool) []string {
	byEndpoint := map[string][]string{}
	for _, d := range v.PendingDecisions() {
		key := d.Request.Thread
		if d.Answered || elsewhere != nil && elsewhere(key) {
			continue
		}
		byEndpoint[string(key.Endpoint)] = append(byEndpoint[string(key.Endpoint)], decisionKind(d.Method)+" in "+threadName(v, key))
	}
	return endpointLines(v, byEndpoint)
}

func (s *Shell) lostContent(v *state.View, final bool) []string {
	byEndpoint := map[string][]string{}
	for _, key := range v.WindowDrafts(s.window) {
		if item := unsentItem(v, key, final); item != "" {
			byEndpoint[string(key.Endpoint)] = append(byEndpoint[string(key.Endpoint)], item)
		}
	}
	return endpointLines(v, byEndpoint)
}

func unsentItem(v *state.View, key state.DraftKey, final bool) string {
	d, queued := v.Draft(key), len(v.FollowUps(key))
	var parts []string
	if (d.Text != "" || len(d.Attachments) > 0) && (!final || v.Unsaved(key)) {
		parts = append(parts, "unsent draft")
	}
	if queued > 0 {
		parts = append(parts, followUpCount(queued))
	}
	if len(parts) == 0 {
		return ""
	}
	return strings.Join(parts, " and ") + " in " + threadName(v, state.ThreadKey{Endpoint: key.Endpoint, Thread: key.Thread})
}

func followUpCount(n int) string {
	if n == 1 {
		return "1 queued follow-up"
	}
	return strconv.Itoa(n) + " queued follow-ups"
}

func endpointLines(v *state.View, byEndpoint map[string][]string) []string {
	var lines []string
	for _, ep := range v.Endpoints() {
		items := byEndpoint[string(ep.ID)]
		if len(items) == 0 {
			continue
		}
		lines = append(lines, "On "+cmpOr(ep.Label, string(ep.ID))+": "+strings.Join(items, "; "))
	}
	return lines
}

func (s *Shell) updateClose(gtx layout.Context, escape bool) bool {
	if !s.close.asking() {
		return false
	}
	if s.closeAnyway.Clicked(gtx) {
		s.close = closeFlow{confirmed: true}
		return true
	}
	if s.keepOpen.Clicked(gtx) || escape {
		s.close = closeFlow{}
		s.focus.request(s.restoreTarget())
	}
	return false
}

func (c closeFlow) title() string {
	var parts []string
	if len(c.decisions) > 0 {
		parts = append(parts, "pending decisions")
	}
	if len(c.unsent) > 0 {
		parts = append(parts, "unsent text")
	}
	if len(c.terminals) > 0 {
		parts = append(parts, "active terminal sessions")
	}
	switch len(parts) {
	case 1:
		switch {
		case len(c.decisions) > 0:
			return decisionsTitle
		case len(c.unsent) > 0:
			return unsentTitle
		default:
			return terminalsTitle
		}
	default:
		return "Close with " + strings.Join(parts, ", ") + "?"
	}
}

func (c closeFlow) lines() []string {
	var lines []string
	if len(c.decisions) > 0 {
		lines = append(append(lines, closeExplanation), c.decisions...)
	}
	if len(c.unsent) > 0 {
		lines = append(append(lines, c.unsentExplanation()), c.unsent...)
	}
	if len(c.terminals) > 0 {
		lines = append(append(lines, terminalsExplanation), c.terminals...)
	}
	return lines
}

func (c closeFlow) unsentExplanation() string {
	if c.final {
		return exitExplanation
	}
	return windowExplanation
}

func (s *Shell) closeCard(gtx layout.Context, f frame) {
	s.card(gtx, f, cardSpec{
		title: s.close.title(),
		lines: s.close.lines(),
		buttons: []button{
			{click: &s.closeAnyway, label: closeAnywayLabel, variant: destructive},
			{click: &s.keepOpen, label: keepOpenLabel, variant: pointed, hue: f.theme.Point(s.open.Thread)},
		},
	})
}
