package tui

import (
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

type dockPosition struct {
	geometry frameGeometry
	settled  int
	column   int
	row      int
	prints   uint64
}

const dockQueryTimeout = time.Second

type dockAnchor struct {
	revision     uint64
	prints       uint64
	landing      bool
	needed       bool
	querying     bool
	query        uint64
	moveRevision uint64
	moving       bool
	position     dockPosition
}

type dockQueryTimeoutMsg struct {
	query uint64
}

type dockLandedMsg struct {
	prints uint64
}

type dockMove struct {
	anchor   *dockAnchor
	revision uint64
	content  string
}

func (move dockMove) String() string {
	if move.anchor.revision != move.revision {
		move.anchor.moving = false
		return ""
	}
	move.anchor.needed = false
	return move.content + dockWriteMarker(move.revision)
}

func (m model) caretLanding() bool {
	return m.out.anchor != nil && m.out.anchor.landing
}

func (m *model) landCaret() tea.Cmd {
	anchor := m.anchorState()
	anchor.landing = true
	prints := anchor.prints
	return tea.Tick(settleDelay, func(time.Time) tea.Msg { return dockLandedMsg{prints: prints} })
}

func (m *model) caretLanded(msg dockLandedMsg) tea.Cmd {
	anchor := m.anchorState()
	if msg.prints != anchor.prints {
		return nil
	}
	if anchor.querying || anchor.moving || m.out.settling {
		return m.landCaret()
	}
	anchor.landing = false
	return nil
}

func (m *model) anchorState() *dockAnchor {
	if m.out.anchor == nil {
		m.out.anchor = new(dockAnchor)
	}
	return m.out.anchor
}

func (m *model) dockPosition() dockPosition {
	view := m.View()
	position := dockPosition{geometry: m.currentGeometry(), prints: m.anchorState().prints, settled: m.out.settleN}
	if view.Cursor != nil {
		position.column = view.Cursor.X
		position.row = view.Cursor.Y
	}
	return position
}

func (m *model) anchorDock() tea.Cmd {
	anchor := m.anchorState()
	if m.dockBlocked(anchor) {
		return nil
	}
	anchor.position = m.dockPosition()
	if anchor.position.geometry.rows == 0 {
		return nil
	}
	anchor.querying = true
	anchor.query++
	generation := anchor.query
	return tea.Batch(
		tea.Raw(dockQueryMarker),
		tea.Tick(dockQueryTimeout, func(time.Time) tea.Msg { return dockQueryTimeoutMsg{query: generation} }),
	)
}

func (m *model) dockBusy() bool {
	return m.out.printing || m.out.settling || m.review != nil || m.leaving
}

func (m *model) dockBlocked(anchor *dockAnchor) bool {
	return !anchor.needed || anchor.querying || anchor.moving || !m.out.started || m.dockBusy()
}

func (m *model) dockStateChanged(anchor *dockAnchor) bool {
	return !anchor.needed || m.dockBusy() || anchor.position != m.dockPosition()
}

func dockCursorOutside(cursor tea.CursorPositionMsg, position dockPosition, height int) bool {
	return cursor.X != position.column || cursor.Y < position.row || cursor.Y >= height
}

func (m *model) queryExpired(msg dockQueryTimeoutMsg) {
	anchor := m.anchorState()
	if !anchor.querying || anchor.query != msg.query {
		return
	}
	anchor.querying = false
	if anchor.position.settled == m.out.settleN {
		anchor.needed = false
	}
}

func (m *model) positionDock(cursor tea.CursorPositionMsg) tea.Cmd {
	anchor := m.anchorState()
	if !anchor.querying {
		return nil
	}
	anchor.querying = false
	if cursor.Y == 0 && anchor.prints > 0 {
		return nil
	}
	if m.dockStateChanged(anchor) {
		return nil
	}
	position := anchor.position
	if dockCursorOutside(cursor, position, m.screen.height) {
		return nil
	}
	distance := m.screen.height - position.geometry.rows + position.row - cursor.Y
	if distance <= 0 {
		anchor.needed = false
		return nil
	}
	anchor.moving = true
	anchor.moveRevision = anchor.revision
	return tea.Raw(dockMove{anchor: anchor, revision: anchor.revision, content: relocateDock(m.View().Content, position, distance)})
}

func relocateDock(content string, position dockPosition, distance int) string {
	var output strings.Builder
	output.WriteString(ansi.HideCursor)
	output.WriteByte('\r')
	if position.row > 0 {
		output.WriteString(ansi.CursorUp(position.row))
	}
	output.WriteString(ansi.EraseEntireLine)
	for range distance - 1 {
		output.WriteString(ansi.CursorDown(1))
		output.WriteString(ansi.EraseEntireLine)
	}
	output.WriteString(ansi.CursorDown(1))
	output.WriteString(ansi.EraseEntireLine)
	output.WriteString(strings.ReplaceAll(content, "\n", "\r\n"+ansi.EraseEntireLine))
	output.WriteByte('\r')
	if above := position.geometry.rows - 1 - position.row; above > 0 {
		output.WriteString(ansi.CursorUp(above))
	}
	if position.column > 0 {
		output.WriteString(ansi.CursorForward(position.column))
	}
	output.WriteString(ansi.ShowCursor)
	return output.String()
}
