package tui

import (
	"bytes"
	"encoding/base64"
	"os"
	"strconv"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

func markEnv(vars map[string]string) func(string) string {
	return func(key string) string {
		return vars[key]
	}
}

func markEnvMsg(vars map[string]string) tea.EnvMsg {
	full := map[string]string{"TERM": "", "TERM_PROGRAM": "", "KITTY_WINDOW_ID": "", "ITERM_SESSION_ID": "", "TMUX": "", "STY": ""}
	for key, value := range vars {
		full[key] = value
	}
	msg := make(tea.EnvMsg, 0, len(full))
	for key, value := range full {
		msg = append(msg, key+"="+value)
	}
	return msg
}

func setMarkEnv(t *testing.T, vars map[string]string) {
	t.Helper()
	for _, key := range []string{"TERM", "TERM_PROGRAM", "KITTY_WINDOW_ID", "ITERM_SESSION_ID", "TMUX", "STY"} {
		value, ok := vars[key]
		if !ok {
			value = ""
		}
		t.Setenv(key, value)
	}
}

func kittyDecoded(t *testing.T, seq string) []byte {
	t.Helper()
	if !strings.HasSuffix(seq, "\x1b\\") {
		t.Fatalf("kitty sequence missing terminator: %q", seq)
	}
	var raw strings.Builder
	for _, seg := range strings.Split(strings.TrimSuffix(seq, "\x1b\\"), "\x1b\\") {
		_, data, ok := strings.Cut(seg, ";")
		if !ok {
			t.Fatalf("kitty chunk holds no data: %q", seg)
		}
		raw.WriteString(data)
	}
	decoded, err := base64.StdEncoding.DecodeString(raw.String())
	if err != nil {
		t.Fatalf("kitty payload is not valid base64: %v", err)
	}
	return decoded
}

func itermDecoded(t *testing.T, seq string) ([]byte, int) {
	t.Helper()
	inner, ok := strings.CutPrefix(seq, "\x1b]1337;File=")
	if !ok || !strings.HasSuffix(seq, "\a") {
		t.Fatalf("iterm sequence framing wrong: %q", seq)
	}
	inner = strings.TrimSuffix(inner, "\a")
	args, data, ok := strings.Cut(inner, ":")
	if !ok {
		t.Fatalf("iterm sequence holds no payload: %q", seq)
	}
	decoded, err := base64.StdEncoding.DecodeString(data)
	if err != nil {
		t.Fatalf("iterm payload is not valid base64: %v", err)
	}
	_, after, ok := strings.Cut(args, ";size=")
	if !ok {
		t.Fatalf("iterm args hold no size: %q", args)
	}
	sizeText, _, _ := strings.Cut(after, ";")
	size, err := strconv.Atoi(sizeText)
	if err != nil {
		t.Fatalf("iterm size is not a number: %q", sizeText)
	}
	return decoded, size
}

func TestDetectMarkProtocol(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		vars map[string]string
		want markProtocol
	}{
		{"empty", map[string]string{}, markNone},
		{"dumb", map[string]string{"TERM": "dumb"}, markNone},
		{"plain", map[string]string{"TERM": "xterm-256color"}, markNone},
		{"unknown program", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "herdr"}, markNone},
		{"tmux suppresses kitty", map[string]string{"TERM": "xterm-kitty", "TMUX": "/tmp/tmux-1"}, markNone},
		{"screen suppresses iterm", map[string]string{"TERM_PROGRAM": "iTerm.app", "STY": "1.pts"}, markNone},
		{"kitty term", map[string]string{"TERM": "xterm-kitty"}, markKitty},
		{"kitty window", map[string]string{"TERM": "xterm-256color", "KITTY_WINDOW_ID": "1"}, markKitty},
		{"kitty program", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "kitty"}, markKitty},
		{"ghostty program", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "ghostty"}, markKitty},
		{"kitty beats iterm", map[string]string{"TERM_PROGRAM": "iTerm.app", "KITTY_WINDOW_ID": "1", "TERM": "xterm-256color"}, markKitty},
		{"iterm", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "iTerm.app"}, markIterm},
		{"wezterm", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "WezTerm"}, markIterm},
		{"iterm session", map[string]string{"TERM": "xterm-256color", "ITERM_SESSION_ID": "1:2"}, markIterm},
		{"sixel term", map[string]string{"TERM": "mlterm-sixel"}, markSixel},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := detectMarkProtocol(markEnv(tc.vars)); got != tc.want {
				t.Fatalf("detectMarkProtocol(%v) = %v, want %v", tc.vars, got, tc.want)
			}
		})
	}
}

func TestMarkEncoderSelection(t *testing.T) {
	cases := []struct {
		name    string
		vars    map[string]string
		product string
		height  int
		want    string
	}{
		{"kitty term", map[string]string{"TERM": "xterm-kitty"}, "dalgona", 16, "kitty"},
		{"kitty window small", map[string]string{"TERM": "xterm-256color", "KITTY_WINDOW_ID": "1"}, "dal", 8, "kitty"},
		{"ghostty", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "ghostty"}, "dalgona", 16, "kitty"},
		{"iterm", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "iTerm.app"}, "dalgona", 16, "iterm"},
		{"wezterm", map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "WezTerm"}, "dal", 8, "iterm"},
		{"iterm session", map[string]string{"TERM": "xterm-256color", "ITERM_SESSION_ID": "1:2"}, "dalgona", 8, "iterm"},
		{"sixel", map[string]string{"TERM": "mlterm-sixel"}, "dalgona", 16, "sixel"},
		{"sixel dal", map[string]string{"TERM": "mlterm-sixel"}, "dal", 8, "sixel"},
		{"none empty", map[string]string{}, "dalgona", 16, ""},
		{"none dumb", map[string]string{"TERM": "dumb"}, "dal", 8, ""},
		{"none plain", map[string]string{"TERM": "xterm-256color"}, "dalgona", 16, ""},
		{"none tmux", map[string]string{"TERM": "xterm-kitty", "TMUX": "/tmp/tmux-1"}, "dalgona", 16, ""},
		{"none screen", map[string]string{"TERM_PROGRAM": "iTerm.app", "STY": "1.pts"}, "dalgona", 16, ""},
		{"none unknown product", map[string]string{"TERM": "xterm-kitty"}, "other", 16, ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			setMarkEnv(t, tc.vars)
			assertMarkEncoding(t, tc.product, tc.height, MarkImage(tc.product, tc.height), tc.want)
		})
	}
}

func assertMarkEncoding(t *testing.T, product string, height int, got, want string) {
	t.Helper()
	prefixes := map[string]string{"kitty": "\x1b_G", "iterm": "\x1b]1337;File=", "sixel": "\x1bP"}
	if want == "" {
		if got != "" {
			t.Fatalf("MarkImage(%q, %d) = %q, want empty", product, height, got)
		}
		return
	}
	if !strings.HasPrefix(got, prefixes[want]) {
		t.Fatalf("MarkImage(%q, %d) = %q, want %s", product, height, got, want)
	}
}

func TestMarkImageWithoutSupportIsEmpty(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "dumb"})
	for _, product := range []string{"dalgona", "dal"} {
		for _, height := range []int{0, 8, 16} {
			if got := MarkImage(product, height); got != "" {
				t.Fatalf("MarkImage(%q, %d) without support = %q, want empty", product, height, got)
			}
		}
	}
}

func TestMarkImageUnknownProductIsEmpty(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "xterm-kitty"})
	if got := MarkImage("other", 16); got != "" {
		t.Fatalf("MarkImage(%q, 16) = %q, want empty", "other", got)
	}
}

func TestMarkImageKitty(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "xterm-kitty"})
	for _, tc := range []struct {
		product string
		height  int
		size    string
		rows    string
	}{
		{"dalgona", 16, "s=256,v=256", "r=16"},
		{"dalgona", 8, "s=128,v=128", "r=8"},
		{"dal", 16, "s=256,v=256", "r=16"},
		{"dal", 0, "s=128,v=128", "r=8"},
	} {
		got := MarkImage(tc.product, tc.height)
		if !strings.HasPrefix(got, "\x1b_Ga=T,f=100,"+tc.size+","+tc.rows) {
			t.Fatalf("MarkImage(%q, %d) kitty prefix = %q", tc.product, tc.height, got)
		}
		if !strings.HasSuffix(got, "\x1b\\") {
			t.Fatalf("MarkImage(%q, %d) kitty suffix = %q", tc.product, tc.height, got)
		}
		if strings.ContainsAny(got, "\a\n") {
			t.Fatalf("MarkImage(%q, %d) kitty holds newline or bell", tc.product, tc.height)
		}
	}
}

func TestMarkKittyBase64(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "xterm-kitty"})
	cases := []struct {
		product string
		height  int
	}{
		{"dalgona", 16},
		{"dalgona", 8},
		{"dal", 16},
		{"dal", 8},
	}
	for _, tc := range cases {
		t.Run(tc.product+"/"+strconv.Itoa(tc.height), func(t *testing.T) {
			got := MarkImage(tc.product, tc.height)
			if got == "" {
				t.Fatalf("MarkImage(%q, %d) is empty", tc.product, tc.height)
			}
			art, ok := markArtFor(tc.product, tc.height)
			if !ok {
				t.Fatalf("markArtFor(%q, %d) missing", tc.product, tc.height)
			}
			if decoded := kittyDecoded(t, got); !bytes.Equal(decoded, art.png) {
				t.Fatalf("MarkImage(%q, %d) kitty payload decodes to %d bytes, want %d", tc.product, tc.height, len(decoded), len(art.png))
			}
		})
	}
}

func TestMarkImageIterm(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "iTerm.app"})
	got := MarkImage("dalgona", 16)
	if !strings.HasPrefix(got, "\x1b]1337;File=") || !strings.HasSuffix(got, "\a") {
		t.Fatalf("MarkImage iterm framing = %q", got)
	}
	if !strings.Contains(got, ";inline=1;height=16:") {
		t.Fatalf("MarkImage iterm args = %q", got)
	}
	small := MarkImage("dalgona", 8)
	if !strings.Contains(small, ";inline=1;height=8:") {
		t.Fatalf("MarkImage iterm small args = %q", small)
	}
}

func TestMarkItermBase64(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "xterm-256color", "TERM_PROGRAM": "iTerm.app"})
	cases := []struct {
		product string
		height  int
	}{
		{"dalgona", 16},
		{"dalgona", 8},
		{"dal", 16},
		{"dal", 8},
	}
	for _, tc := range cases {
		t.Run(tc.product+"/"+strconv.Itoa(tc.height), func(t *testing.T) {
			got := MarkImage(tc.product, tc.height)
			if got == "" {
				t.Fatalf("MarkImage(%q, %d) is empty", tc.product, tc.height)
			}
			art, ok := markArtFor(tc.product, tc.height)
			if !ok {
				t.Fatalf("markArtFor(%q, %d) missing", tc.product, tc.height)
			}
			decoded, size := itermDecoded(t, got)
			if !bytes.Equal(decoded, art.png) {
				t.Fatalf("MarkImage(%q, %d) iterm payload decodes to %d bytes, want %d", tc.product, tc.height, len(decoded), len(art.png))
			}
			if size != len(art.png) {
				t.Fatalf("MarkImage(%q, %d) iterm size = %d, want %d", tc.product, tc.height, size, len(art.png))
			}
		})
	}
}

func TestMarkImageSixel(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "mlterm-sixel"})
	for _, product := range []string{"dalgona", "dal"} {
		got := MarkImage(product, 16)
		if !strings.HasPrefix(got, "\x1bP") || !strings.HasSuffix(got, "\x1b\\") {
			t.Fatalf("MarkImage(%q, 16) sixel framing = %q", product, got)
		}
		if strings.ContainsAny(got, "\a\n") {
			t.Fatalf("MarkImage(%q, 16) sixel holds newline or bell", product)
		}
	}
}

func TestMarkSixelIntroducer(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "mlterm-sixel"})
	cases := []struct {
		product string
		height  int
	}{
		{"dalgona", 16},
		{"dalgona", 8},
		{"dal", 16},
		{"dal", 8},
	}
	for _, tc := range cases {
		t.Run(tc.product+"/"+strconv.Itoa(tc.height), func(t *testing.T) {
			got := MarkImage(tc.product, tc.height)
			if !strings.HasPrefix(got, "\x1bP") {
				t.Fatalf("MarkImage(%q, %d) missing sixel introducer: %q", tc.product, tc.height, got)
			}
			if !strings.HasSuffix(got, "\x1b\\") {
				t.Fatalf("MarkImage(%q, %d) missing terminator: %q", tc.product, tc.height, got)
			}
		})
	}
}

func TestMarkPayloadReachesPrintPathWhole(t *testing.T) {
	setMarkEnv(t, map[string]string{"TERM": "xterm-kitty"})
	m := newTestModel(nil, Options{Cwd: "/work"})
	mark := MarkImage("dalgona", 16)
	if mark == "" {
		t.Fatal("MarkImage with kitty support is empty")
	}
	rows := m.screen.wrap(mark, terminalWidth(m.screen.width))
	if len(rows) != 1 || rows[0] != mark {
		t.Fatalf("wrap splits the mark into %d rows", len(rows))
	}
	if got := m.screen.emit(mark); got != mark {
		t.Fatalf("emit changes the mark: %q", got)
	}
	if got := m.screen.cells(mark); got != 0 {
		t.Fatalf("mark cells = %d, want 0", got)
	}
}

func TestWelcomeMarkRows(t *testing.T) {
	t.Parallel()
	if got := welcomeMarkRows(80); got != markLargeRows {
		t.Fatalf("welcomeMarkRows(80) = %d, want %d", got, markLargeRows)
	}
	if got := welcomeMarkRows(20); got != markSmallRows {
		t.Fatalf("welcomeMarkRows(20) = %d, want %d", got, markSmallRows)
	}
}

func TestWelcomeLinesPrependsMark(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{Cwd: "/work"})
	m.conn.product = "dalgona"
	m.screen.env = markEnvMsg(map[string]string{"TERM": "xterm-kitty"})
	lines := m.welcomeLines()
	if len(lines) != 3 {
		t.Fatalf("welcome lines = %d, want 3", len(lines))
	}
	if lines[0] != m.welcomeMark() || lines[0] == "" {
		t.Fatal("welcome first line is not the mark")
	}
}

func TestWelcomeLinesWithoutSupportUnchanged(t *testing.T) {
	t.Parallel()
	m := newTestModel(nil, Options{Cwd: "/work"})
	m.conn.product = "dalgona"
	m.screen.env = markEnvMsg(map[string]string{"TERM": "xterm-256color"})
	lines := m.welcomeLines()
	if len(lines) != 2 {
		t.Fatalf("welcome lines = %d, want 2", len(lines))
	}
	if strings.Contains(lines[0], "\x1bP") || strings.Contains(lines[0], "\x1b_G") || strings.Contains(lines[0], "\x1b]") {
		t.Fatalf("welcome line holds image sequences: %q", lines[0])
	}
}

func TestWelcomeUnsupportedMatchesCurrentOutput(t *testing.T) {
	t.Parallel()
	m := initializedModel(t, "/work")
	m.screen.env = markEnvMsg(map[string]string{"TERM": "dumb"})
	lines := m.welcomeLines()
	if len(lines) != 2 {
		t.Fatalf("welcome lines = %d, want 2", len(lines))
	}
	stripped := []string{ansi.Strip(lines[0]), ansi.Strip(lines[1])}
	want := []string{"dalgona 1.2.3 · default · /work", "Type a prompt · / commands · ? keys"}
	for i := range want {
		if stripped[i] != want[i] {
			t.Fatalf("welcome line %d = %q, want %q", i, stripped[i], want[i])
		}
	}
}

func TestMarkEmbeddedMatchesBrand(t *testing.T) {
	t.Parallel()
	pairs := map[string][]byte{
		"../../../assets/brand/dalgona-8.png":    markDalgonaSmallPNG,
		"../../../assets/brand/dalgona-16.png":   markDalgonaLargePNG,
		"../../../assets/brand/dal-8.png":        markDalSmallPNG,
		"../../../assets/brand/dal-16.png":       markDalLargePNG,
		"../../../assets/brand/dalgona-8.sixel":  markDalgonaSmallSixel,
		"../../../assets/brand/dalgona-16.sixel": markDalgonaLargeSixel,
		"../../../assets/brand/dal-8.sixel":      markDalSmallSixel,
		"../../../assets/brand/dal-16.sixel":     markDalLargeSixel,
	}
	for path, embedded := range pairs {
		raw, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read %s: %v", path, err)
		}
		if !bytes.Equal(raw, embedded) {
			t.Fatalf("embedded %s differs from %s", path, path)
		}
	}
}
