package tui

import (
	"bytes"
	"encoding/base64"
	"image"
	"image/color"
	"image/png"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/rpc"
)

func pngBytes(t *testing.T, width, height int) []byte {
	t.Helper()
	img := image.NewRGBA(image.Rect(0, 0, width, height))
	img.Set(0, 0, color.RGBA{R: 255, A: 255})
	var out bytes.Buffer
	if err := png.Encode(&out, img); err != nil {
		t.Fatalf("encode png: %v", err)
	}
	return out.Bytes()
}

func writePNG(t *testing.T, path string, width, height, size int) string {
	t.Helper()
	data := pngBytes(t, width, height)
	if size > len(data) {
		data = append(data, make([]byte, size-len(data))...)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
	return path
}

func imageModel(t *testing.T, caller *fakeCaller, modalities ...string) model {
	t.Helper()
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m.thread.modelName = "gpt-test"
	m.thread.inputModalities = map[string][]string{"gpt-test": modalities}
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	return m
}

func TestImagePathPasteBecomesChipAndInputItem(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := imageModel(t, caller, "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting an image path started no read")
	if !strings.Contains(dockText(m), "[Image #1 3x2]") {
		t.Fatalf("composer does not show the image chip:\n%s", dockText(m))
	}
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the image")
	if len(caller.calls) == 0 || caller.calls[len(caller.calls)-1].method != "turn/start" {
		t.Fatalf("calls = %#v, want turn/start last", caller.calls)
	}
	input := caller.calls[len(caller.calls)-1].params.(rpc.TurnStartParams).Input
	if len(input) != 1 || !strings.HasPrefix(input[0].URL, "data:image/png;base64,") {
		t.Fatalf("sent %#v, want one image input with a data:image/png;base64 URL", input)
	}
}

func TestImagePathPasteOverLimitNotice(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "big.png"), 3, 2, 5<<20+1)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting an image path started no read")
	if want := "Image big.png is 5242881 bytes, over the 5 MiB limit"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
	if got := len(m.input.composer.images()); got != 0 {
		t.Fatalf("composer holds %d image chips, want none", got)
	}
}

func TestImagePathPasteRejectsWebPWithItsType(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := filepath.Join(t.TempDir(), "shot.webp")
	webp := []byte("RIFF\x00\x00\x00\x00WEBPVP8 ")
	if err := os.WriteFile(path, webp, 0o600); err != nil {
		t.Fatalf("write webp: %v", err)
	}
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting a webp path started no read")
	if want := "unsupported image type image/webp; send PNG, JPEG or GIF"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestClipboardToolMissingNotice(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("the missing-tool notice is the Linux branch")
	}
	t.Setenv("WAYLAND_DISPLAY", "")
	t.Setenv("DISPLAY", "")
	m := imageModel(t, new(fakeCaller), "text", "image")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: 'v', Mod: tea.ModCtrl})
	if want := "image paste needs wl-paste, xclip or pngpaste"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestImageRefusedWithoutImageModality(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text")
	m, command := drive(t, m, tea.KeyPressMsg{Code: 'v', Mod: tea.ModCtrl})
	if command != nil {
		t.Fatal("ctrl+v started a clipboard read for a text-only model")
	}
	if want := "this model does not accept images"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestImagesOverTenMiBRefused(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m := imageModel(t, new(fakeCaller), "text", "image")
	const fourMiB = 4 << 20
	for i := range 3 {
		path := writePNG(t, filepath.Join(dir, "shot"+string(rune('a'+i))+".png"), 3, 2, fourMiB)
		var command tea.Cmd
		m, command = drive(t, m, tea.PasteMsg{Content: path})
		m = runCommand(t, m, command, "pasting an image path started no read")
	}
	if got := len(m.input.composer.images()); got != 2 {
		t.Fatalf("composer holds %d image chips, want 2 under the 10 MiB request limit", got)
	}
	if want := "images total 12582912 bytes; the limit is 10485760 bytes (10 MiB) per request"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestImageLimitTenPerPrompt(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	small := base64.StdEncoding.EncodeToString(pngBytes(t, 1, 1))
	for range 10 {
		m, _ = drive(t, m, imageMsg{mime: "image/png", data: small, width: 1, height: 1})
	}
	if got := len(m.input.composer.images()); got != 10 {
		t.Fatalf("composer holds %d image chips, want 10", got)
	}
	m, _ = drive(t, m, imageMsg{mime: "image/png", data: small, width: 1, height: 1})
	if want := "at most 10 images per prompt"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
	if got := len(m.input.composer.images()); got != 10 {
		t.Fatalf("eleventh image was attached: %d chips", got)
	}
}

func TestImagePathParsing(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name, text, want string
		ok               bool
	}{
		{name: "plain", text: "/tmp/a.png", want: "/tmp/a.png", ok: true},
		{name: "quoted", text: `'/tmp/a b.PNG'`, want: "/tmp/a b.PNG", ok: true},
		{name: "file url", text: "file:///tmp/a.jpeg", want: "/tmp/a.jpeg", ok: true},
		{name: "escaped space", text: `/tmp/a\ b.gif`, want: `/tmp/a\ b.gif`, ok: true},
		{name: "not an image", text: "/tmp/notes.txt", ok: false},
		{name: "two lines", text: "/tmp/a.png\n/tmp/b.png", ok: false},
	}
	for _, tt := range tests {
		got, ok := imagePath(tt.text)
		if ok != tt.ok || got != tt.want {
			t.Errorf("%s: imagePath(%q) = %q, %v; want %q, %v", tt.name, tt.text, got, ok, tt.want, tt.ok)
		}
	}
}

func TestImageEscapedSpacePathResolves(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	dir := t.TempDir()
	writePNG(t, filepath.Join(dir, "a b.png"), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: filepath.Join(dir, `a\ b.png`)})
	m = runCommand(t, m, command, "pasting an escaped path started no read")
	if !strings.Contains(dockText(m), "[Image #1 3x2]") {
		t.Fatalf("escaped path did not become a chip:\n%s", dockText(m))
	}
}

func TestImageLiteralBackslashPathIsKept(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("a file name cannot hold a backslash on Windows")
	}
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), `a\ b.png`), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting a literal path started no read")
	if !strings.Contains(dockText(m), "[Image #1 3x2]") {
		t.Fatalf("literal path with a backslash did not become a chip:\n%s", dockText(m))
	}
}

func TestUnescapePasteRemovesOnlyEscapes(t *testing.T) {
	t.Parallel()
	tests := []struct{ name, in, want string }{
		{name: "escaped space", in: `a\ b`, want: "a b"},
		{name: "escaped quotes", in: `a\'b\"c`, want: `a'b"c`},
		{name: "escaped parentheses", in: `a\(b\)`, want: "a(b)"},
		{name: "backslash before a letter stays", in: `a\bc`, want: `a\bc`},
		{name: "drive path stays", in: `C:\Users\me\shot.png`, want: `C:\Users\me\shot.png`},
		{name: "double backslash stays", in: `a\\b`, want: `a\\b`},
	}
	for _, tt := range tests {
		if got := unescapePaste(tt.in); got != tt.want {
			t.Errorf("%s: unescapePaste(%q) = %q, want %q", tt.name, tt.in, got, tt.want)
		}
	}
}

func TestClipboardImageOnlyAtPrompt(t *testing.T) {
	t.Setenv("WAYLAND_DISPLAY", "")
	t.Setenv("DISPLAY", "")
	tests := []struct {
		name  string
		setup func(*model)
	}{
		{name: "list panel open", setup: func(m *model) {
			m.list = &listPanel{title: "models", items: []listItem{{id: "a", label: "a"}}}
		}},
		{name: "answer mode", setup: func(m *model) { m.input.mode = inputAnswer }},
	}
	for _, tt := range tests {
		m := imageModel(t, new(fakeCaller), "text", "image")
		tt.setup(&m)
		if command := m.clipboardImage(); command != nil || m.notice != "" {
			t.Errorf("%s: ctrl+v started a clipboard read (command set: %t) or set notice %q", tt.name, command != nil, m.notice)
		}
	}
}

func TestImageChipSurvivesEditBeforeRead(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	for range len([]rune(path)) {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	}
	m = runCommand(t, m, command, "pasting an image path started no read")
	if got := len(m.input.composer.images()); got != 1 {
		t.Fatalf("composer holds %d image chips after the pasted text was edited away, want 1:\n%s", got, dockText(m))
	}
}

func TestImageSendsTextBeforeImage(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := imageModel(t, caller, "text", "image")
	for _, r := range "describe" {
		m, _ = drive(t, m, press(r, 0, string(r)))
	}
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting an image path started no read")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the prompt")
	if len(caller.calls) == 0 || caller.calls[len(caller.calls)-1].method != "turn/start" {
		t.Fatalf("calls = %#v, want turn/start last", caller.calls)
	}
	input := caller.calls[len(caller.calls)-1].params.(rpc.TurnStartParams).Input
	if len(input) != 2 || input[0].Type != "text" || input[0].Text != "describe" || input[1].Type != "image" {
		t.Fatalf("sent %#v, want the text describe, then one image", input)
	}
}
