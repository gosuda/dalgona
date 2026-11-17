package tui

import (
	"strings"
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
		{map[string]string{"KITTY_WINDOW_ID": "1"}, "kitty"},
		{map[string]string{"TERM": "xterm-kitty"}, "kitty"},
		{map[string]string{"TERM": "xterm-ghostty"}, "kitty"},
		{map[string]string{"WEZTERM_PANE": "0"}, "kitty"},
		{map[string]string{"TERM_PROGRAM": "iTerm.app"}, "iterm"},
		{map[string]string{"TERM_PROGRAM": "vscode"}, ""},
		{map[string]string{"TERM": "xterm-256color"}, ""},
		{map[string]string{"KITTY_WINDOW_ID": "1", "TMUX": "/tmp/tmux-1"}, ""},
		{map[string]string{"TERM": "xterm-kitty", "STY": "1.pts-0.host"}, ""},
	}
	for _, c := range cases {
		if got := detectImageProto(envMap(c.env)); got != c.want {
			t.Fatalf("env %v: proto %q want %q", c.env, got, c.want)
		}
	}
}

func TestLogoSeqEmptyWithoutProto(t *testing.T) {
	t.Parallel()
	if got := logoSeq("dalgona", "", theme{}); got != "" {
		t.Fatalf("no proto must yield no logo: %q", got)
	}
}

func TestLogoSeqKittyChunks(t *testing.T) {
	t.Parallel()
	seq := logoSeq("dalgona", "kitty", theme{})
	if !strings.HasPrefix(seq, imgRowMark) {
		t.Fatalf("logo row must carry the image mark: %q", seq[:8])
	}
	body := seq[len(imgRowMark):]
	if !strings.HasPrefix(body, "\x1b_Ga=T,f=100,t=d,q=2,C=1,c=10,r=8,m=1;") {
		t.Fatalf("kitty head chunk: %q", body[:64])
	}
	if strings.Count(body, "\x1b_G") < 2 {
		t.Fatalf("dalgona png must be chunked: %d apc", strings.Count(body, "\x1b_G"))
	}
	if !strings.Contains(body, "\x1b_Gm=0;") || !strings.HasSuffix(body, strings.Repeat("\n", 8)) {
		t.Fatalf("kitty tail chunk or cursor advance missing")
	}
}

func TestLogoSeqDalTinted(t *testing.T) {
	t.Parallel()
	seq := logoSeq("dal", "iterm", theme{name: themeNight, known: true})
	if !strings.Contains(seq, "\x1b]1337;File=inline=1") {
		t.Fatalf("iterm seq: %q", seq[:64])
	}
	if tinted, err := tintPNG(dalPNG, "#FF8800"); err != nil || len(tinted) == 0 {
		t.Fatalf("tintPNG: %v %d", err, len(tinted))
	}
}

func TestTakeChunkImageRowBypassesWrap(t *testing.T) {
	t.Parallel()
	m := &model{}
	m.screen.width = 20
	raw := "\x1b_Ga=T;QUJD\x1b\\" + strings.Repeat("\n", 8)
	m.out.pending = []string{imgRowMark + raw, "after"}
	rows := m.takeChunk(5)
	if len(rows) != 1 || rows[0] != raw {
		t.Fatalf("image row must come out alone and unwrapped: %#v", rows)
	}
	if m.out.pending[0] != "after" {
		t.Fatalf("pending after image row: %#v", m.out.pending)
	}
}

func TestNextChunkImageSkipsEmit(t *testing.T) {
	t.Parallel()
	m := &model{}
	m.screen.width = 80
	raw := "\x1b_Ga=T;QUJD\x1b\\"
	m.out.pending = []string{imgRowMark + raw}
	text, ok := m.nextChunk(5)
	if !ok || text != raw {
		t.Fatalf("nextChunk image: ok=%v %q", ok, text)
	}
}
