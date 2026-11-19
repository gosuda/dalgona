package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func BenchmarkUpdateKeyPress(b *testing.B) {
	for _, terminal := range []struct {
		name  string
		proto string
	}{
		{name: "plain", proto: ""},
		{name: "kitty", proto: "kitty"},
	} {
		for _, draft := range []struct {
			name  string
			bytes int
		}{
			{name: "empty", bytes: 0},
			{name: "1KiB", bytes: 1 << 10},
			{name: "64KiB", bytes: 64 << 10},
		} {
			b.Run(terminal.name+"/"+draft.name, func(b *testing.B) {
				m := newTestModel(nil, Options{})
				m.conn.product = "dal"
				m.screen.width, m.screen.height = 80, 40
				m.screen.imgProto = terminal.proto
				m.input.composer.SetWidth(80)
				m.input.composer.SetValue(strings.Repeat("word ", draft.bytes/5))
				m = playTerminal(m)
				typed := press('a', 0, "a")
				erased := press(tea.KeyBackspace, 0, "")
				b.ReportAllocs()
				b.ResetTimer()
				for i := range b.N {
					key := typed
					if i%2 == 1 {
						key = erased
					}
					updated, _ := m.Update(key)
					m = updated.(model)
					_ = m.View()
				}
			})
		}
	}
}
