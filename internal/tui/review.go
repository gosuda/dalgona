package tui

import (
	"cmp"
	"fmt"
	"slices"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
)

const reviewTitleCells = 40

type reviewState struct {
	index int
	view  viewport.Model
	live  *entry
}

func newReviewView() viewport.Model {
	view := viewport.New()
	view.KeyMap = viewport.KeyMap{
		Up:       key.NewBinding(key.WithKeys("up", "k")),
		Down:     key.NewBinding(key.WithKeys("down", "j")),
		PageUp:   key.NewBinding(key.WithKeys("pgup")),
		PageDown: key.NewBinding(key.WithKeys("pgdown")),
	}
	return view
}

func (m *model) openReview() {
	if m.review != nil {
		return
	}
	live := m.liveEntry()
	if live == nil && len(m.out.entries) == 0 {
		m.noticef("nothing to review yet")
		return
	}
	m.review = &reviewState{live: live, view: newReviewView()}
	m.setReviewEntry(len(m.reviewEntries()) - 1)
}

func (m *model) closeReview() {
	m.review = nil
}

func (m *model) liveEntry() *entry {
	if len(m.approvals) > 0 {
		open := m.approvals[0]
		snapshot := newEntry("approval:"+open.itemID, m.approvalLines(open))
		return &snapshot
	}
	if len(m.live.items) == 0 {
		return nil
	}
	item := m.live.items[len(m.live.items)-1].snapshot()
	snapshot := newEntry("live:"+item.ID, m.renderItem(item, verbosityHigh))
	return &snapshot
}

func (m *model) reviewEntries() []entry {
	if m.review == nil || m.review.live == nil {
		return m.out.entries
	}
	return append(slices.Clone(m.out.entries), *m.review.live)
}

func (m *model) setReviewEntry(index int) {
	r := m.review
	entries := m.reviewEntries()
	r.index = min(max(index, 0), len(entries)-1)
	m.fitReview()
	r.view.GotoTop()
}

func (m *model) fitReview() {
	r := m.review
	entries := m.reviewEntries()
	if r == nil || len(entries) == 0 {
		return
	}
	r.index = min(r.index, len(entries)-1)
	width := terminalWidth(m.screen.width)
	r.view.SetWidth(width)
	r.view.SetHeight(reviewBodyRows(m.screen.height))
	r.view.SetContentLines(m.reviewLines(entries[r.index].text, width))
}

func reviewBodyRows(height int) int {
	return max(1, height-3)
}

func (m *model) reviewLines(text string, width int) []string {
	var lines []string
	for line := range strings.SplitSeq(text, "\n") {
		lines = append(lines, m.screen.wrap(line, width)...)
	}
	return lines
}

func (m *model) reviewView() string {
	r := m.review
	entries := m.reviewEntries()
	width := terminalWidth(m.screen.width)
	title := cmp.Or(m.screen.fit(entries[r.index].title, reviewTitleCells), "row")
	head := fmt.Sprintf("Review · %s · %d/%d", title, r.index+1, len(entries))
	if n := len(m.out.pending); n > 0 {
		head += fmt.Sprintf(" · %d waiting", n)
	}
	foot := "↑↓ scroll · [ ] items · Esc close"
	return strings.Join([]string{
		m.screen.theme.paint(roleText, m.screen.fit(head, width)),
		r.view.View(),
		m.screen.theme.paint(roleDim, m.screen.fit(foot, width)),
	}, "\n")
}

func (m *model) reviewKey(msg tea.KeyPressMsg) {
	switch msg.String() {
	case "esc", "q", "ctrl+o":
		m.closeReview()
	case "[":
		m.setReviewEntry(m.review.index - 1)
	case "]":
		m.setReviewEntry(m.review.index + 1)
	case "home", "g":
		m.review.view.GotoTop()
	case "end", "G":
		m.review.view.GotoBottom()
	default:
		m.scrollReview(msg)
	}
}

func (m *model) scrollReview(msg tea.Msg) {
	if m.review == nil {
		return
	}
	m.review.view, _ = m.review.view.Update(msg)
}
