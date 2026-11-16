package tui

import (
	"os"
)

func (m *model) markGetenv(key string) string {
	if m.screen.env != nil {
		if value, ok := m.screen.env.LookupEnv(key); ok {
			return value
		}
	}
	return os.Getenv(key)
}

func (m *model) welcomeMark() string {
	return markImageWith(m.markGetenv, m.conn.product, welcomeMarkRows(m.screen.width))
}

func welcomeMarkRows(width int) int {
	if terminalWidth(width) < 40 {
		return markSmallRows
	}
	return markLargeRows
}
