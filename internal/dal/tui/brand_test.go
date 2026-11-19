package tui

import (
	"testing"
)

func envMap(kv map[string]string) func(string) string {
	return func(k string) string { return kv[k] }
}

func TestDetectImageProto(t *testing.T) {
	t.Parallel()
	cases := []struct {
		env  map[string]string
		want string
	}{
		{map[string]string{"TERM": "xterm-256color", "KITTY_WINDOW_ID": "1"}, "kitty"},
		{map[string]string{"TERM": "xterm-kitty"}, "kitty"},
		{map[string]string{"TERM": "xterm-ghostty"}, "kitty"},
		{map[string]string{"TERM": "xterm-256color", "WEZTERM_PANE": "0"}, "kitty"},
		{map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "iTerm.app"}, "iterm"},
		{map[string]string{"TERM": "xterm-256color", "ITERM_SESSION_ID": "w0t0p0:ABC"}, "iterm"},
		{map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "vscode"}, ""},
		{map[string]string{"TERM": "xterm-256color"}, ""},
		{map[string]string{"TERM": "xterm-256color", "KITTY_WINDOW_ID": "1", "TMUX": "/tmp/tmux-1"}, ""},
		{map[string]string{"TERM": "xterm-kitty", "STY": "1.pts-0.host"}, ""},
		{map[string]string{"KITTY_WINDOW_ID": "1"}, ""},
		{map[string]string{"TERM": "", "KITTY_WINDOW_ID": "1"}, ""},
		{map[string]string{"TERM": "dumb", "KITTY_WINDOW_ID": "1"}, ""},
		{map[string]string{"TERM": "dumb", "WEZTERM_PANE": "0"}, ""},
		{map[string]string{"TERM": "dumb", "TERM_PROGRAM": "iTerm.app"}, ""},
		{map[string]string{"TERM": "dumb", "ITERM_SESSION_ID": "w0t0p0:ABC"}, ""},
		{map[string]string{"ITERM_SESSION_ID": "w0t0p0:ABC"}, ""},
		{map[string]string{"TERM": "xterm-256color", "ITERM_SESSION_ID": "w0t0p0:ABC", "TMUX": "/tmp/tmux-1"}, ""},
	}
	for _, c := range cases {
		if got := detectImageProto(envMap(c.env)); got != c.want {
			t.Errorf("env %v: proto %q want %q", c.env, got, c.want)
		}
	}
}

func TestBrandPNG(t *testing.T) {
	t.Parallel()
	if len(brandPNG("dalgona", theme{})) == 0 {
		t.Fatal("dalgona brand png empty")
	}
	if len(brandPNG("dal", theme{name: themeNight, known: true})) == 0 {
		t.Fatal("dal brand png empty")
	}
	if brandPNG("other", theme{}) != nil {
		t.Fatal("unknown product must have no brand png")
	}
}

func TestTintPNG(t *testing.T) {
	t.Parallel()
	if tinted, err := tintPNG(dalPNG, "#FF8800"); err != nil || len(tinted) == 0 {
		t.Fatalf("tintPNG: %v %d", err, len(tinted))
	}
}
