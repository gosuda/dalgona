package tui

import (
	"fmt"
	"time"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
)

const (
	liveOnGlyph   = "\u25aa"
	liveOffGlyph  = "\u25ab"
	liveFrameRate = 500 * time.Millisecond
	motionTick    = time.Second
	hintWindow    = 2000 * time.Millisecond
)

func newSpinner(motion bool) spinner.Model {
	if !motion {
		return spinner.New(spinner.WithSpinner(spinner.Spinner{Frames: []string{liveOnGlyph}, FPS: motionTick}))
	}
	return spinner.New(spinner.WithSpinner(spinner.Spinner{Frames: []string{liveOnGlyph, liveOffGlyph}, FPS: liveFrameRate}))
}

func (m *model) applyMotion(msg tea.EnvMsg) {
	m.screen.motion = msg.Getenv("DAL_NO_MOTION") == ""
	m.screen.spin = newSpinner(m.screen.motion)
}

func (m *model) now() time.Time {
	if m.conn.clock == nil {
		return time.Now()
	}
	return m.conn.clock()
}

func (m *model) turnLive() bool {
	switch m.thread.state {
	case stateRunning, stateAwaitingApproval, "awaiting input":
		return true
	}
	_, waiting := m.retryRemaining()
	return waiting
}

func (m *model) scheduleTick() tea.Cmd {
	if m.screen.ticking || !m.turnLive() {
		return nil
	}
	m.screen.ticking = true
	return m.screen.spin.Tick
}

func (m *model) updateTick(msg spinner.TickMsg) tea.Cmd {
	spin, command := m.screen.spin.Update(msg)
	m.screen.spin = spin
	if command == nil || !m.turnLive() {
		m.screen.ticking = false
		return nil
	}
	return command
}

func (m *model) retryRemaining() (time.Duration, bool) {
	if m.live.retryUntil.IsZero() {
		return 0, false
	}
	left := m.live.retryUntil.Sub(m.now())
	return left, left > 0
}

func retryLine(left time.Duration) string {
	seconds := int(left / time.Second)
	return fmt.Sprintf("rate limited · retrying in %dm %02ds · esc stops", seconds/60, seconds%60)
}

func (m *model) expireHint(msg hintExpiredMsg) {
	if msg.id == m.hint.id {
		m.hint.text, m.hint.pending = "", ""
	}
}
