package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func TestMatchKeywordsBoundariesAndMasking(t *testing.T) {
	t.Parallel()
	table := []Keyword{{Word: "ultrathink", Hue: [2]int{0, 330}}}
	tests := []struct {
		name   string
		prompt string
		want   bool
	}{
		{name: "standalone", prompt: "ultrathink", want: true},
		{name: "comma", prompt: "ultrathink,", want: true},
		{name: "uppercase", prompt: "ULTRATHINK"},
		{name: "suffix", prompt: "ultrathinking"},
		{name: "property", prompt: "x.ultrathink"},
		{name: "method", prompt: "ultrathink()"},
		{name: "path", prompt: "/ultrathink"},
		{name: "sentence period", prompt: "ultrathink.", want: true},
		{name: "extension", prompt: "ultrathink.md"},
		{name: "one colon", prompt: "x:ultrathink", want: true},
		{name: "namespace", prompt: "x::ultrathink"},
		{name: "inline code", prompt: "`ultrathink`"},
		{name: "double inline code", prompt: "``ultrathink``"},
		{name: "fenced code", prompt: "```\nultrathink\n```"},
		{name: "tilde fence", prompt: "~~~text\nultrathink\n~~~"},
		{name: "indented fence", prompt: "   ```go\nultrathink\n   ```"},
		{name: "xml", prompt: "<system>ultrathink</system>"},
		{name: "nested xml", prompt: "<x>before <x>ultrathink</x></x>"},
		{name: "html comment", prompt: "<!-- ultrathink -->"},
		{name: "after code", prompt: "`code` ultrathink", want: true},
		{name: "unicode before", prompt: "éultrathink"},
		{name: "unicode separated", prompt: "é ultrathink", want: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			got := len(matchKeywords(test.prompt, table)) == 1
			if got != test.want {
				t.Errorf("matchKeywords(%q) = %v, want %v", test.prompt, got, test.want)
			}
		})
	}
}

func TestKeywordGlowUsesGradientAndPhase(t *testing.T) {
	t.Parallel()
	table := []Keyword{{Word: "ultrathink", Hue: [2]int{0, 330}}}
	static := glowKeywords("ultrathink, please", table, 0)
	shifted := glowKeywords("ultrathink, please", table, 1.0/14.0)
	if static == shifted {
		t.Fatal("shimmer phase did not change the gradient")
	}
	if got := stripANSI(static); got != "ultrathink, please" {
		t.Fatalf("glow changed visible text to %q", got)
	}
	if !strings.Contains(static, "\x1b[38;2;245;71;71mu") {
		t.Fatalf("gradient color is reset before the first rune: %q", static)
	}
	if strings.Contains(glowKeywords("`ultrathink`", table, 0), "\x1b[") {
		t.Fatal("inline code received keyword colors")
	}
	if len(keywordPalette(table[0].Hue)) != 14 {
		t.Fatal("keyword palette does not have 14 stops")
	}
}

func TestModelSchedulesShimmerOnlyForVisibleMatch(t *testing.T) {
	t.Parallel()
	m := newModel(nil, Options{}, nil)
	m.input.keywords = []Keyword{{Word: "ultrathink", Hue: [2]int{0, 330}}}
	m.input.composer.SetValue("plain prompt")
	m, command := drive(t, m, tea.PasteMsg{Content: " still plain"})
	if command != nil {
		t.Fatal("plain composer scheduled a shimmer tick")
	}
	m.input.composer.SetValue("ultrathink")
	m, command = drive(t, m, tea.PasteMsg{Content: ""})
	if command == nil {
		t.Fatal("visible keyword did not schedule a shimmer tick")
	}
	m, command = drive(t, m, tea.BlurMsg{})
	if command != nil {
		t.Fatal("blurred composer scheduled a new shimmer tick")
	}
	m.input.shimmerActive = true
	m, command = drive(t, m, shimmerMsg{})
	if command != nil || m.input.shimmerActive {
		t.Fatal("blurred composer kept the shimmer timer active")
	}
}

func TestComposerAndSentMessageKeywordGlow(t *testing.T) {
	t.Parallel()
	table := []Keyword{{Word: "ultrathink", Hue: [2]int{0, 330}}}
	m := newModel(nil, Options{}, nil)
	m.screen.width = 8
	m.input.composer.SetWidth(8)
	m.input.keywords = table
	m.input.composer.SetValue("ultrathink")
	view := m.View().Content
	if !strings.Contains(view, "\x1b[38;2;245;71;71mu") {
		t.Fatalf("composer broken across lines lost keyword color:\n%q", view)
	}
	if got := strings.ReplaceAll(stripANSI(view), "\n", ""); !strings.Contains(got, "ultrathink") {
		t.Fatalf("wrapped composer lost keyword text:\n%s", got)
	}
	settled := m.renderSettledItem(rpc.Item{Type: "userMessage", Text: "ultrathink, please"})
	if !strings.Contains(settled, "\x1b[38;2;") {
		t.Fatalf("settled user message lost keyword color: %q", settled)
	}
}

func stripANSI(text string) string {
	var out strings.Builder
	for i := 0; i < len(text); {
		if text[i] != '\x1b' {
			out.WriteByte(text[i])
			i++
			continue
		}
		i += 2
		for i < len(text) && text[i] != 'm' {
			i++
		}
		if i < len(text) {
			i++
		}
	}
	return out.String()
}
