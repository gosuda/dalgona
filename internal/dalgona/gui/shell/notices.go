//go:build gui

package shell

import (
	"fmt"
	"image"
	"slices"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	noticeLimit   = 5
	errorsLabel   = "Errors"
	statusLabel   = "Status"
	openThreadAct = "Open thread"
)

type noticeAction uint8

const (
	actDismiss noticeAction = iota
	actRetry
	actReconnect
	actCheck
	actRestore
	actOpen
	actionCount
)

var actionLabels = [actionCount]string{
	actDismiss:   "Dismiss",
	actRetry:     "Retry",
	actReconnect: "Reconnect",
	actCheck:     "Check",
	actRestore:   "Restore message",
	actOpen:      openThreadAct,
}

type noticeEntry struct {
	id       string
	text     string
	failure  bool
	actions  []noticeAction
	notice   uint64
	op       string
	endpoint client.EndpointID
	thread   state.ThreadKey
	local    uint64
}

type localNotice struct {
	id      uint64
	text    string
	failure bool
}

type noticeStack struct {
	locals    []localNotice
	serial    uint64
	dismissed map[string]bool
	clicks    map[string]*[actionCount]widget.Clickable
	announced map[string]bool
	entries   []noticeEntry
}

func newNoticeStack() noticeStack {
	return noticeStack{
		dismissed: map[string]bool{},
		clicks:    map[string]*[actionCount]widget.Clickable{},
		announced: map[string]bool{},
	}
}

func (n *noticeStack) local(text string, tone state.NoticeTone) {
	n.serial++
	n.locals = slices.DeleteFunc(n.locals, func(l localNotice) bool { return l.text == text })
	n.locals = append(n.locals, localNotice{id: n.serial, text: text, failure: tone == state.NoticeError})
	if over := len(n.locals) - noticeLimit; over > 0 {
		n.locals = slices.Delete(n.locals, 0, over)
	}
}

func (s *Shell) buildNotices(f frame) []noticeEntry {
	var out []noticeEntry
	out = append(out, waitingEntry(s, f)...)
	out = append(out, connectionEntries(f.view, s.notices.dismissed)...)
	out = append(out, mutationEntries(f.view, s.window)...)
	for _, n := range f.view.Notices() {
		out = append(out, noticeEntry{id: fmt.Sprintf("notice:%d", n.ID), text: n.Text, failure: n.Tone == state.NoticeError, actions: []noticeAction{actDismiss}, notice: n.ID})
	}
	for _, l := range s.notices.locals {
		out = append(out, noticeEntry{id: fmt.Sprintf("local:%d", l.id), text: l.text, failure: l.failure, actions: []noticeAction{actDismiss}, local: l.id})
	}
	return boundNotices(out)
}

func boundNotices(entries []noticeEntry) []noticeEntry {
	var kept []noticeEntry
	for i, e := range entries {
		later := slices.ContainsFunc(entries[i+1:], func(o noticeEntry) bool { return o.text == e.text })
		if !later {
			kept = append(kept, e)
		}
	}
	if over := len(kept) - noticeLimit; over > 0 {
		kept = kept[over:]
	}
	return kept
}

func waitingEntry(s *Shell, f frame) []noticeEntry {
	var first state.ThreadKey
	count := 0
	for _, key := range f.view.Threads() {
		if key == s.open {
			continue
		}
		tv, _ := f.view.Thread(key)
		for _, r := range tv.Requests {
			if !r.Answered && r.Actionable {
				count++
				first = cmpKey(first, key)
			}
		}
	}
	if count == 0 {
		return nil
	}
	tv, _ := f.view.Thread(first)
	ep, _ := endpointOf(f.view, first.Endpoint)
	where := threadTitle(tv.Thread) + " on " + cmpOr(ep.Label, string(ep.ID))
	text := fmt.Sprintf("%s waiting on another thread: %s.", requestPhrase(count), where)
	return []noticeEntry{{id: "waiting", text: text, actions: []noticeAction{actOpen}, thread: first}}
}

func cmpKey(current, next state.ThreadKey) state.ThreadKey {
	if current.Thread == "" {
		return next
	}
	return current
}

func requestPhrase(n int) string {
	if n == 1 {
		return "1 request"
	}
	return fmt.Sprintf("%d requests", n)
}

func connectionEntries(v *state.View, dismissed map[string]bool) []noticeEntry {
	var out []noticeEntry
	for _, ep := range v.Endpoints() {
		text, ok := connectionText(ep)
		id := fmt.Sprintf("connection:%s:%d:%d", ep.ID, ep.Link.Generation, ep.Phase)
		if !ok || dismissed[id] {
			continue
		}
		e := noticeEntry{id: id, text: text, failure: true, endpoint: ep.ID}
		if reconnectable(ep.Phase) {
			e.actions = append(e.actions, actReconnect)
		}
		e.actions = append(e.actions, actDismiss)
		out = append(out, e)
	}
	return out
}

func connectionText(ep state.EndpointView) (string, bool) {
	label := cmpOr(ep.Label, string(ep.ID))
	cause := "no cause was reported"
	if ep.Err != nil {
		cause = ep.Err.Error()
	}
	switch ep.Phase {
	case client.PhaseOffline:
		return label + " is offline after its reconnect attempts. Reconnect to load its threads and send to it.", true
	case client.PhaseFailed:
		return "Could not connect to " + label + ": " + cause, true
	case client.PhaseIncompatible:
		return label + " cannot serve this desktop: " + cause, true
	case client.PhaseIdle, client.PhaseConnecting, client.PhaseConnected, client.PhaseReconnecting:
		return "", false
	}
	return "", false
}

var mutationActions = map[string]string{
	"turn/start":     "send the message",
	"turn/steer":     "steer the turn",
	"turn/interrupt": "interrupt the turn",
}

func mutationEntries(v *state.View, window state.WindowID) []noticeEntry {
	var out []noticeEntry
	for _, mu := range v.Mutations() {
		if mu.Key.Window != window {
			continue
		}
		ep, _ := endpointOf(v, mu.Key.Endpoint)
		if e, ok := mutationEntry(mu, ep); ok {
			out = append(out, e)
		}
	}
	return out
}

func mutationEntry(mu state.MutationView, ep state.EndpointView) (noticeEntry, bool) {
	action := cmpOr(mutationActions[mu.Method], mu.Method)
	label := cmpOr(ep.Label, string(ep.ID))
	live := ep.Phase == client.PhaseConnected
	e := noticeEntry{id: "op:" + mu.OperationID, op: mu.OperationID, endpoint: mu.Key.Endpoint, failure: true}
	switch mu.State {
	case state.MutationFailed:
		e.text = fmt.Sprintf("Could not %s on %s: %v", action, label, mu.Err)
		e.actions = append(e.actions, choose(live, actRetry, actReconnect))
	case state.MutationUnknown:
		e.text, e.actions = unknownEntry(mu, action, label, live)
	case state.MutationReconciling:
		e.text, e.failure = fmt.Sprintf("Checking whether %s reached %s…", action, label), false
		return e, true
	case state.MutationWaiting:
		if live {
			return e, false
		}
		e.text, e.failure = fmt.Sprintf("Waiting for %s to connect to %s.", label, action), false
		e.actions = append(e.actions, actReconnect)
	case state.MutationDispatched:
		return e, false
	default:
		return e, false
	}
	e.actions = appendRestoreAction(e.actions, mu)
	e.actions = append(e.actions, actDismiss)
	return e, true
}

func appendRestoreAction(actions []noticeAction, mu state.MutationView) []noticeAction {
	if mu.State != state.MutationUnknown && (mu.Payload.Text != "" || len(mu.Payload.Attachments) > 0) {
		return append(actions, actRestore)
	}
	return actions
}

func unknownEntry(mu state.MutationView, action, label string, live bool) (string, []noticeAction) {
	text := fmt.Sprintf("%s lost the connection before confirming whether it could %s.", label, action)
	if !mu.Receipt {
		return text + " Check the thread before sending it again.", nil
	}
	return text + " Check its receipt before sending again.", []noticeAction{choose(live, actCheck, actReconnect)}
}

func choose(cond bool, yes, no noticeAction) noticeAction {
	if cond {
		return yes
	}
	return no
}

func (n *noticeStack) controls(id string) *[actionCount]widget.Clickable {
	c, ok := n.clicks[id]
	if !ok {
		c = new([actionCount]widget.Clickable)
		n.clicks[id] = c
	}
	return c
}

func (n *noticeStack) update(gtx layout.Context, s *Shell, f frame) {
	n.entries = s.buildNotices(f)
	live := map[string]bool{}
	for _, e := range n.entries {
		live[e.id] = true
		live[e.id+"\x00"+e.text] = true
		c := n.controls(e.id)
		for _, a := range e.actions {
			if c[a].Clicked(gtx) {
				s.act(e, a)
			}
		}
		n.announce(s, e)
	}
	for id := range n.clicks {
		if !live[id] {
			delete(n.clicks, id)
		}
	}
	for id := range n.announced {
		if !live[id] {
			delete(n.announced, id)
		}
	}
}

func (n *noticeStack) announce(s *Shell, e noticeEntry) {
	key := e.id + "\x00" + e.text
	if n.announced[key] {
		return
	}
	n.announced[key] = true
	kind := platform.AnnounceProgress
	switch {
	case e.failure:
		kind = platform.AnnounceFailure
	case e.id == "waiting":
		kind = platform.AnnounceDecision
	}
	s.announce(platform.Announcement{Kind: kind, Text: e.text})
}

func (s *Shell) act(e noticeEntry, a noticeAction) {
	switch a {
	case actRetry:
		s.submit(state.Retry{OperationID: e.op})
	case actReconnect:
		s.submit(state.Connect{Endpoint: e.endpoint})
	case actCheck:
		s.submit(state.Check{OperationID: e.op})
	case actRestore:
		s.submit(state.Restore{OperationID: e.op})
		s.focus.request(s.restoreTarget())
	case actOpen:
		s.navigate(e.thread, false)
	case actDismiss:
		s.dismiss(e)
	case actionCount:
		return
	}
}

func (s *Shell) dismiss(e noticeEntry) {
	switch {
	case e.notice != 0:
		s.submit(state.DismissNotice{ID: e.notice})
	case e.op != "":
		s.submit(state.Dismiss{OperationID: e.op})
	case e.local != 0:
		s.notices.locals = slices.DeleteFunc(s.notices.locals, func(l localNotice) bool { return l.id == e.local })
	default:
		s.notices.dismissed[e.id] = true
	}
	s.focus.request(s.restoreTarget())
}

func (n *noticeStack) layout(gtx layout.Context, s *Shell, f frame) layout.Dimensions {
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions { return n.channel(gtx, s, f, true) }),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions { return n.channel(gtx, s, f, false) }),
	)
}

func (n *noticeStack) channel(gtx layout.Context, s *Shell, f frame, failures bool) layout.Dimensions {
	label, latest := statusLabel, s.channels.status
	if failures {
		label, latest = errorsLabel, s.channels.alert
	}
	if latest != "" {
		label += ": " + latest
	}
	gtx.Constraints.Min = image.Pt(gtx.Constraints.Max.X, 0)
	return labelled(gtx, label, func(gtx layout.Context) layout.Dimensions {
		var children []layout.FlexChild
		for _, e := range n.entries {
			if e.failure == failures {
				children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions { return n.row(gtx, s, f, e) }))
			}
		}
		dims := layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
		dims.Size = image.Pt(gtx.Constraints.Max.X, max(dims.Size.Y, 1))
		return dims
	})
}

func (n *noticeStack) row(gtx layout.Context, s *Shell, f frame, e noticeEntry) layout.Dimensions {
	p := f.palette
	st := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Text}
	if e.failure {
		st.color = p.FailedText
	}
	c := n.controls(e.id)
	items := []layout.Widget{func(gtx layout.Context) layout.Dimensions {
		gtx.Constraints.Max.X = min(gtx.Constraints.Max.X, dp(gtx, theme.TranscriptMaxWidth))
		return drawText(gtx, s.shaper, st, e.text)
	}}
	for _, a := range e.actions {
		items = append(items, func(gtx layout.Context) layout.Dimensions {
			return s.control(gtx, p, button{click: &c[a], label: actionLabels[a]})
		})
	}
	inset := layout.Inset{Top: logical(theme.Space2), Bottom: logical(theme.Space2), Left: logical(theme.Space6), Right: logical(theme.Space6)}
	return inset.Layout(gtx, func(gtx layout.Context) layout.Dimensions { return wrapRow(gtx, theme.Space3, items...) })
}
