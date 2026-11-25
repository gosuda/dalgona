package tui

import (
	"slices"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

var osc9Terminals = []string{"iTerm.app", "WezTerm", "ghostty"}

func (m *model) windowTitle() string {
	name := m.conn.product
	if m.conn.machine != "" {
		name += " · " + m.conn.machine
	}
	return sanitizeText(name + " · " + m.thread.title + " · " + stateWord(m.thread.state, m.thread.lastTurn))
}

func (m *model) notifyLine(what string) string {
	name := m.conn.product
	if m.conn.machine != "" {
		name = m.conn.machine
	}
	return name + ": " + what + " · " + m.thread.title
}

func (m *model) notify(text string) tea.Cmd {
	if !m.screen.notifications || m.screen.focused {
		return nil
	}
	return tea.Raw(notifySequence(m.screen.env, sanitizeText(text)))
}

func notifySequence(env tea.EnvMsg, text string) string {
	switch {
	case env.Getenv("TMUX") != "":
		return "\a"
	case env.Getenv("TERM") == "xterm-kitty":
		return ansi.DesktopNotification(text)
	case slices.Contains(osc9Terminals, env.Getenv("TERM_PROGRAM")):
		return ansi.Notify(text)
	default:
		return "\a"
	}
}
