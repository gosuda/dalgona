//go:build gui

package shell

import (
	"context"
	"slices"
	"strings"
	"sync"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	tagSeparator = "\x1f"
	sendQueue    = 16
)

type Notifier interface {
	Permission() platform.NotificationPermission
	Request(ctx context.Context) (platform.NotificationPermission, error)
	Refresh(ctx context.Context) (platform.NotificationPermission, error)
	Send(ctx context.Context, n platform.Notification) error
	OpenSettings() error
}

type attentionJob struct {
	request  bool
	settings bool
	refresh  bool
	note     platform.Notification
}

type Attention struct {
	notifier   Notifier
	saved      func(bool)
	wake       func()
	jobs       chan attentionJob
	mu         sync.Mutex
	enabled    bool
	permission platform.NotificationPermission
	requesting bool
	refreshing bool
	problem    string

	observing sync.Mutex
	seen      map[string]bool
	running   map[string]bool
}
type NotificationRow struct {
	attention *Attention
	allow     widget.Clickable
	toggle    widget.Clickable
	settings  widget.Clickable
}

func NewAttention(n Notifier, enabled bool, saved func(bool), wake func()) *Attention {
	a := &Attention{notifier: n, saved: saved, wake: wake, enabled: enabled, jobs: make(chan attentionJob, sendQueue), seen: map[string]bool{}, running: map[string]bool{}}
	a.permission = platform.PermissionUnavailable
	if n != nil {
		a.permission = n.Permission()
	}
	return a
}

func ParseTag(tag string) (state.ThreadKey, bool) {
	endpoint, thread, ok := strings.Cut(tag, tagSeparator)
	if !ok || endpoint == "" || thread == "" {
		return state.ThreadKey{}, false
	}
	return state.ThreadKey{Endpoint: client.EndpointID(endpoint), Thread: thread}, true
}

func notificationTag(key state.ThreadKey) string {
	return string(key.Endpoint) + tagSeparator + key.Thread
}

func (a *Attention) Run(ctx context.Context) {
	for {
		select {
		case <-ctx.Done():
			return
		case job := <-a.jobs:
			a.perform(ctx, job)
		}
	}
}

func (a *Attention) perform(ctx context.Context, job attentionJob) {
	var err error
	switch {
	case job.request:
		var granted platform.NotificationPermission
		granted, err = a.notifier.Request(ctx)
		a.mu.Lock()
		a.permission, a.requesting = granted, false
		a.enabled = err == nil && granted == platform.PermissionGranted
		enabled := a.enabled
		a.mu.Unlock()
		if a.saved != nil {
			a.saved(enabled)
		}
	case job.refresh:
		var now platform.NotificationPermission
		now, err = a.notifier.Refresh(ctx)
		a.mu.Lock()
		a.permission, a.refreshing = now, false
		a.mu.Unlock()
	case job.settings:
		err = a.notifier.OpenSettings()
	default:
		err = a.notifier.Send(ctx, job.note)
	}
	a.mu.Lock()
	a.problem = ""
	if err != nil {
		a.problem = err.Error()
	}
	a.mu.Unlock()
	if a.wake != nil {
		a.wake()
	}
}

func (a *Attention) Refresh() {
	if a.notifier == nil {
		return
	}
	a.mu.Lock()
	if a.refreshing {
		a.mu.Unlock()
		return
	}
	a.refreshing = true
	a.mu.Unlock()
	if !a.queue(attentionJob{refresh: true}) {
		a.mu.Lock()
		a.refreshing = false
		a.mu.Unlock()
	}
}

func (a *Attention) queue(job attentionJob) bool {
	select {
	case a.jobs <- job:
		return true
	default:
		return false
	}
}

type attentionEvent struct {
	id   string
	kind platform.TurnEvent
}

var outcomeEvents = map[string]platform.TurnEvent{
	"completed":   platform.TurnCompleted,
	"failed":      platform.TurnFailed,
	"interrupted": platform.TurnInterrupted,
}

func (a *Attention) events(key state.ThreadKey, tv *state.ThreadView) []attentionEvent {
	prefix := notificationTag(key) + tagSeparator
	var out []attentionEvent
	for _, r := range tv.Requests {
		if !r.Answered && r.Actionable {
			out = append(out, attentionEvent{id: prefix + "request" + tagSeparator + r.Key.Request, kind: platform.TurnWaiting})
		}
	}
	for _, t := range tv.Turns {
		turn := prefix + "turn" + tagSeparator + t.ID
		if t.Status == "inProgress" {
			a.running[turn] = true
			continue
		}
		if kind, ok := outcomeEvents[t.Status]; ok && a.running[turn] {
			out = append(out, attentionEvent{id: turn + tagSeparator + t.Status, kind: kind})
		}
	}
	return out
}

func (a *Attention) Observe(v *state.View, visible func(state.ThreadKey) bool) {
	a.observing.Lock()
	defer a.observing.Unlock()
	a.prune(v)
	for _, key := range v.Threads() {
		tv, ok := v.Thread(key)
		if !ok {
			continue
		}
		ep, _ := endpointOf(v, key.Endpoint)
		name := cmpOr(tv.Thread.Name, "Untitled thread") + " · " + cmpOr(ep.Label, string(ep.ID))
		for _, ev := range a.events(key, tv) {
			a.notify(ev, key, name, visible != nil && visible(key))
		}
	}
}

func (a *Attention) prune(v *state.View) {
	var live []string
	for _, key := range v.Threads() {
		live = append(live, notificationTag(key)+tagSeparator)
	}
	for id := range a.seen {
		if !taggedWith(id, live) {
			delete(a.seen, id)
		}
	}
	for id := range a.running {
		if !taggedWith(id, live) {
			delete(a.running, id)
		}
	}
}

func taggedWith(id string, prefixes []string) bool {
	return slices.ContainsFunc(prefixes, func(prefix string) bool { return strings.HasPrefix(id, prefix) })
}

func (a *Attention) notify(ev attentionEvent, key state.ThreadKey, name string, visible bool) {
	if a.seen[ev.id] {
		return
	}
	a.mu.Lock()
	st := platform.NotificationState{Permission: a.permission, Enabled: a.enabled, Focused: visible}
	a.mu.Unlock()
	n, ok := st.Notice(ev.kind, notificationTag(key), name)
	if !ok {
		a.seen[ev.id] = true
		return
	}
	if !a.queue(attentionJob{note: n}) {
		return
	}
	a.seen[ev.id] = true
	for id := range a.running {
		if strings.HasPrefix(ev.id, id+tagSeparator) {
			delete(a.running, id)
		}
	}
}

type rowAction uint8

const (
	noAction rowAction = iota
	allowAction
	settingsAction
	toggleAction
)

type attentionRow struct {
	text    string
	action  rowAction
	label   string
	busy    bool
	problem string
}

func (a *Attention) row() attentionRow {
	a.mu.Lock()
	defer a.mu.Unlock()
	r := attentionRow{busy: a.requesting, problem: a.problem}
	switch {
	case a.notifier == nil || a.permission == platform.PermissionUnavailable:
		r.text = "Desktop notifications are unavailable on this system"
	case a.permission == platform.PermissionDenied:
		r.text, r.action, r.label = "Desktop notifications are blocked in system settings", settingsAction, "Open settings"
	case a.permission == platform.PermissionGranted && a.enabled:
		r.text, r.action, r.label = "Desktop notifications are on", toggleAction, "Turn off"
	case a.permission == platform.PermissionGranted:
		r.text, r.action, r.label = "Desktop notifications are off", toggleAction, "Turn on"
	default:
		r.text, r.action, r.label = "Desktop notifications are off", allowAction, "Allow desktop notifications"
	}
	return r
}

func (a *Attention) requestPermission() {
	a.mu.Lock()
	start := !a.requesting && a.permission == platform.PermissionUndecided
	a.requesting = start
	a.mu.Unlock()
	if start && !a.queue(attentionJob{request: true}) {
		a.mu.Lock()
		a.requesting = false
		a.mu.Unlock()
	}
}

func (a *Attention) flip() {
	a.mu.Lock()
	a.enabled = !a.enabled
	enabled := a.enabled
	a.mu.Unlock()
	if a.saved != nil {
		a.saved(enabled)
	}
}

func (a *Attention) Row() *NotificationRow { return &NotificationRow{attention: a} }

func (r *NotificationRow) update(gtx layout.Context) {
	if r.allow.Clicked(gtx) {
		r.attention.requestPermission()
	}
	if r.settings.Clicked(gtx) {
		r.attention.queue(attentionJob{settings: true})
	}
	if r.toggle.Clicked(gtx) {
		r.attention.flip()
	}
}

func (r *NotificationRow) click(action rowAction) *widget.Clickable {
	switch action {
	case allowAction:
		return &r.allow
	case settingsAction:
		return &r.settings
	case toggleAction:
		return &r.toggle
	case noAction:
	}
	return nil
}

func (r *NotificationRow) Layout(gtx layout.Context, env Env) layout.Dimensions {
	r.update(gtx)
	row := r.attention.row()
	p := env.Theme.Palette()
	st := textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Dim}
	items := []layout.Widget{func(gtx layout.Context) layout.Dimensions { return drawText(gtx, env.Shaper, st, row.text) }}
	if click := r.click(row.action); click != nil {
		items = append(items, func(gtx layout.Context) layout.Dimensions {
			return drawButton(gtx, env.Shaper, p, button{click: click, label: row.label, busy: row.busy, note: busyNote(row.busy)})
		})
	}
	if row.problem != "" {
		bad := st
		bad.color = p.FailedText
		items = append(items, func(gtx layout.Context) layout.Dimensions { return drawText(gtx, env.Shaper, bad, row.problem) })
	}
	return wrapRow(gtx, theme.Space3, items...)
}
