//go:build gui

package shell

import (
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type focusTrack struct {
	seen        map[event.Tag]bool
	focusedNow  event.Tag
	focusedLast event.Tag
	pending     event.Tag
}

func newFocusTrack() focusTrack {
	return focusTrack{seen: map[event.Tag]bool{}}
}

func (t *focusTrack) begin(gtx layout.Context) {
	clear(t.seen)
	t.focusedNow = nil
	if t.pending != nil {
		gtx.Execute(key.FocusCmd{Tag: t.pending})
		t.pending = nil
	}
}

func (t *focusTrack) drawn(gtx layout.Context, tag event.Tag) {
	t.seen[tag] = true
	if gtx.Focused(tag) {
		t.focusedNow = tag
	}
}

func (t *focusTrack) request(tag event.Tag) {
	t.pending = tag
}

func (t *focusTrack) usable(tag event.Tag) bool {
	return tag != nil && t.seen[tag]
}

func (t *focusTrack) end(gtx layout.Context, fallback event.Tag) {
	lost := t.focusedLast != nil && !t.seen[t.focusedLast] && gtx.Focused(t.focusedLast)
	if lost && fallback != nil {
		gtx.Execute(key.FocusCmd{Tag: fallback})
	}
	t.focusedLast = t.focusedNow
}

type channels struct {
	announcer platform.Announcer
	status    string
	alert     string
	last      platform.Announcement
}

func (s *Shell) announce(n platform.Announcement) {
	s.deliver(s.channels.announcer.Announce(s.now, n))
}

func (s *Shell) flushAnnouncements() {
	s.deliver(s.channels.announcer.Flush(s.now))
}

func (s *Shell) deliver(n platform.Announcement, ok bool) {
	if !ok || n == s.channels.last {
		return
	}
	s.channels.last = n
	if n.Kind == platform.AnnounceFailure {
		s.channels.alert = n.Text
	} else {
		s.channels.status = n.Text
	}
	if s.announced != nil {
		s.announced(n)
	}
}
