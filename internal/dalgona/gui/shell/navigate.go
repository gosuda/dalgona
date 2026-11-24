//go:build gui

package shell

import (
	"fmt"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	discardLabel = "Discard follow-ups and open"
	cancelLabel  = "Cancel"
)

type switchAsk struct {
	to    state.ThreadKey
	count int
	child bool
}

type pendingSwitch struct {
	from   state.ThreadKey
	to     state.ThreadKey
	notice uint64
}

type switching struct {
	ask     *switchAsk
	pending *pendingSwitch
	discard widget.Clickable
	cancel  widget.Clickable
}

func (w switching) asking() bool { return w.ask != nil }

func (s *Shell) queued(key state.ThreadKey) int {
	if s.view == nil || key.Thread == "" {
		return 0
	}
	return len(s.view.FollowUps(s.draftKey(key)))
}

func (s *Shell) navigate(target state.ThreadKey, child bool) {
	if target == s.open || target.Thread == "" || s.switching.pending != nil {
		return
	}
	if n := s.queued(s.open); n > 0 {
		s.switching.ask = &switchAsk{to: target, count: n, child: child}
		s.focus.request(&s.switching.cancel)
		return
	}
	s.switchTo(target, child, false)
}

func (s *Shell) switchTo(target state.ThreadKey, child, discard bool) {
	from := s.open
	s.back = state.ThreadKey{}
	if child {
		s.back = from
	}
	s.open = target
	s.menu.open = false
	s.submit(state.OpenView{Window: s.window, Thread: target})
	if from.Thread == "" {
		return
	}
	if !discard {
		s.submit(state.CloseView{Window: s.window, Thread: from})
		return
	}
	s.switching.pending = &pendingSwitch{from: from, to: target, notice: newestNotice(s.view)}
}

func newestNotice(v *state.View) uint64 {
	var id uint64
	if v == nil {
		return id
	}
	for _, n := range v.Notices() {
		id = max(id, n.ID)
	}
	return id
}

func opened(tv *state.ThreadView) bool {
	return !tv.Opening && tv.Err == nil && !tv.Missing && (tv.HistoryID != "" || tv.Thread.ID != "")
}

func failedOpen(tv *state.ThreadView) bool {
	return !tv.Opening && (tv.Err != nil || tv.Missing)
}

func refused(v *state.View, p *pendingSwitch) bool {
	for _, n := range v.Notices() {
		if n.ID > p.notice && n.Endpoint == p.to.Endpoint && n.Thread == p.to.Thread {
			return true
		}
	}
	return false
}

func (s *Shell) settleSwitch() {
	p := s.switching.pending
	if p == nil {
		return
	}
	tv, ok := s.view.Thread(p.to)
	switch {
	case ok && opened(tv):
		s.submit(state.DiscardFollowUps{Key: s.draftKey(p.from)})
		s.submit(state.CloseView{Window: s.window, Thread: p.from})
		s.switching.pending = nil
	case ok && failedOpen(tv):
		s.submit(state.CloseView{Window: s.window, Thread: p.to})
		s.revertSwitch(p, tv.Err)
	case !ok && refused(s.view, p):
		s.revertSwitch(p, nil)
	}
}

func (s *Shell) revertSwitch(p *pendingSwitch, cause error) {
	s.switching.pending = nil
	if s.open == p.to {
		s.open, s.back = p.from, state.ThreadKey{}
	}
	text := "The thread did not open, so its queued follow-ups stay with the current thread"
	if cause != nil {
		text = fmt.Sprintf("The thread did not open (%v), so the queued follow-ups stay with the current thread", cause)
	}
	s.notices.local(text, state.NoticeError)
}

func (s *Shell) updateSwitch(gtx layout.Context, escape bool) {
	ask := s.switching.ask
	if ask == nil {
		return
	}
	if s.switching.discard.Clicked(gtx) {
		s.switching.ask = nil
		s.switchTo(ask.to, ask.child, true)
		return
	}
	if s.switching.cancel.Clicked(gtx) || escape {
		s.switching.ask = nil
	}
}

func (s *Shell) switchCard(gtx layout.Context, f frame) {
	ask := s.switching.ask
	target := ask.to.Thread
	if tv, ok := f.view.Thread(ask.to); ok && tv.Thread.ID != "" {
		target = threadTitle(tv.Thread)
	}
	for _, l := range f.lanes {
		for _, r := range l.rows {
			if r.key == ask.to {
				target = r.title
			}
		}
	}
	body := fmt.Sprintf("%s has %s. Opening %s discards them after it opens; Cancel keeps this thread and its queue.", s.openTitle(f), queuedPhrase(ask.count), target)
	s.card(gtx, f, cardSpec{
		title: "Discard queued follow-ups?",
		lines: []string{body},
		buttons: []button{
			{click: &s.switching.discard, label: discardLabel, variant: destructive},
			{click: &s.switching.cancel, label: cancelLabel, variant: pointed},
		},
	})
}

func queuedPhrase(n int) string {
	if n == 1 {
		return "1 queued follow-up"
	}
	return fmt.Sprintf("%d queued follow-ups", n)
}
