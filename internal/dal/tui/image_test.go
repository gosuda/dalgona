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

	"github.com/gosuda/dalgona/internal/dal/rpc"
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

func TestImagePathPasteEndingInCRLeavesOnlyTheChip(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path + "\r"})
	m = runCommand(t, m, command, "pasting an image path started no read")
	if got := len(m.input.composer.chips); got != 1 {
		t.Fatalf("composer holds %d chips, want the image chip", got)
	}
	if got := []rune(m.input.composer.Value()); len(got) != 1 {
		t.Fatalf("composer holds %q, want the chip alone", string(got))
	}
}

func TestImagePathPasteEndingInLFOrCRLFLeavesOnlyTheChip(t *testing.T) {
	t.Parallel()
	for _, tail := range []string{"\n", "\r\n"} {
		m := imageModel(t, new(fakeCaller), "text", "image")
		path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
		m, command := drive(t, m, tea.PasteMsg{Content: path + tail})
		m = runCommand(t, m, command, "pasting an image path started no read")
		if got := len(m.input.composer.chips); got != 1 {
			t.Fatalf("tail %q: composer holds %d chips, want the image chip", tail, got)
		}
		if got := []rune(m.input.composer.Value()); len(got) != 1 {
			t.Fatalf("tail %q: composer holds %q, want the chip alone", tail, string(got))
		}
	}
}

func TestPastedPathSplitByALineBreakStaysText(t *testing.T) {
	t.Parallel()
	for _, sep := range []string{"\r", "\n", "\r\n"} {
		m := imageModel(t, new(fakeCaller), "text", "image")
		dir := t.TempDir()
		writePNG(t, filepath.Join(dir, "shot.png"), 3, 2, 0)
		head := filepath.Join(dir, "shot")
		m, command := drive(t, m, tea.PasteMsg{Content: head + sep + ".png"})
		if command != nil {
			m, _ = drive(t, m, command())
		}
		if got := len(m.input.composer.chips); got != 0 {
			t.Fatalf("separator %q: composer holds %d chips, want the two lines as text", sep, got)
		}
		if got, want := m.input.composer.Value(), head+"\n.png"; got != want {
			t.Fatalf("separator %q: composer holds %q, want %q", sep, got, want)
		}
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

func writeClipboardUtilities(t *testing.T, dir string) {
	t.Helper()
	const utility = "#!/bin/sh\nprintf invoked > \"$CLIPBOARD_TEST_MARKER\"\n/bin/cat \"$CLIPBOARD_TEST_IMAGE\"\n"
	for _, name := range []string{"wl-paste", "pngpaste"} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(utility), 0o700); err != nil {
			t.Fatalf("write clipboard utility: %v", err)
		}
	}
}

func TestImageRefusedWithoutImageModality(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the clipboard utility fixture needs a POSIX shell")
	}
	dir := t.TempDir()
	marker := filepath.Join(dir, "invoked")
	image := writePNG(t, filepath.Join(dir, "clipboard.png"), 3, 2, 0)
	t.Setenv("PATH", dir)
	t.Setenv("WAYLAND_DISPLAY", "clipboard-test")
	t.Setenv("DISPLAY", "")
	t.Setenv("CLIPBOARD_TEST_MARKER", marker)
	t.Setenv("CLIPBOARD_TEST_IMAGE", image)
	writeClipboardUtilities(t, dir)
	m := imageModel(t, new(fakeCaller), "text")
	m, command := drive(t, m, tea.KeyPressMsg{Code: 'v', Mod: tea.ModCtrl})
	if command != nil {
		m = runCommand(t, m, command, "")
	}
	if want := "this model does not accept images"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatalf("text-only clipboard invocation marker: %v, want file absent", err)
	}
	m = imageModel(t, new(fakeCaller), "text", "image")
	m, command = drive(t, m, tea.KeyPressMsg{Code: 'v', Mod: tea.ModCtrl})
	m = runCommand(t, m, command, "image-capable clipboard paste started no command")
	if got, err := os.ReadFile(marker); err != nil || string(got) != "invoked" {
		t.Fatalf("image-capable clipboard marker = %q, %v; want invoked", got, err)
	}
	if !strings.Contains(dockText(m), "[Image #1 3x2]") {
		t.Fatalf("clipboard image made no chip; notice %q; dock:\n%s", m.notice, dockText(m))
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

func pasteDraft(t *testing.T, m model, content string) model {
	t.Helper()
	m, command := drive(t, m, tea.PasteMsg{Content: content})
	if command != nil {
		m, _ = drive(t, m, command())
	}
	if got := len(m.input.composer.images()); got != 0 {
		t.Fatalf("setup paste %q made %d image chips, want plain text", content, got)
	}
	return m
}

func draftShown(m model) string {
	return m.input.composer.display(m.input.composer.text)
}

func assertDraftAndCaret(t *testing.T, m model, want string, caret int) {
	t.Helper()
	if got := draftShown(m); got != want {
		t.Fatalf("composer holds %q, want %q", got, want)
	}
	if got := m.input.composer.cursor; got != caret {
		t.Fatalf("caret at rune %d, want %d", got, caret)
	}
	if got := len(m.input.composer.images()); got != 1 {
		t.Fatalf("composer holds %d image chips, want 1", got)
	}
}

func TestImagePathPasteReplacesOnlyThePastedRange(t *testing.T) {
	t.Parallel()
	const label = "[Image #1 3x2]"
	tests := []struct {
		name   string
		draft  string
		left   int
		pasted string
		before string
		after  string
	}{
		{name: "line break at the end", draft: "Reference: %s\nCompare with: ", pasted: "%s\n", before: "Reference: %s\nCompare with: "},
		{name: "line break in the middle", draft: "Reference: %s\nCompare with: ", left: 14, pasted: "%s\n", before: "Reference: %s\n", after: "Compare with: "},
		{name: "CRLF in the middle", draft: "Reference: %s\nCompare with: ", left: 14, pasted: "%s\r\n", before: "Reference: %s\n", after: "Compare with: "},
		{name: "no line break at the end", draft: "Reference: %s Compare with: ", pasted: "%s", before: "Reference: %s Compare with: "},
		{name: "no line break in the middle", draft: "Reference: %s Compare with: ", left: 14, pasted: "%s", before: "Reference: %s ", after: "Compare with: "},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			m := imageModel(t, new(fakeCaller), "text", "image")
			path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
			m = pasteDraft(t, m, strings.ReplaceAll(test.draft, "%s", path))
			for range test.left {
				m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyLeft})
			}
			m, command := drive(t, m, tea.PasteMsg{Content: strings.ReplaceAll(test.pasted, "%s", path)})
			m = runCommand(t, m, command, "pasting an image path started no read")
			before := strings.ReplaceAll(test.before, "%s", path)
			assertDraftAndCaret(t, m, before+label+test.after, len([]rune(before))+1)
		})
	}
}

func TestImageChipKeepsTextTypedAfterThePasteWhileTheReadRuns(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m = pasteDraft(t, m, "Reference: "+path+"\nCompare with: ")
	m, command := drive(t, m, tea.PasteMsg{Content: path + "\n"})
	m = typeText(t, m, "ok")
	m = runCommand(t, m, command, "pasting an image path started no read")
	before := "Reference: " + path + "\nCompare with: "
	assertDraftAndCaret(t, m, before+"[Image #1 3x2]ok", len([]rune(before))+1+len("ok"))
}

func TestImageChipGoesToTheCaretWhenTheDraftShiftedBeforeTheRead(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m = pasteDraft(t, m, "Reference: "+path+"\nCompare with: ")
	m, command := drive(t, m, tea.PasteMsg{Content: path + "\n"})
	for m.input.composer.cursor > 0 {
		m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyLeft})
	}
	m = typeText(t, m, "X")
	m = runCommand(t, m, command, "pasting an image path started no read")
	assertDraftAndCaret(t, m, "X[Image #1 3x2]Reference: "+path+"\nCompare with: "+path+"\n", 2)
}

func TestImageChipGoesToTheCaretWhenThePastedTextWasEditedBeforeTheRead(t *testing.T) {
	t.Parallel()
	m := imageModel(t, new(fakeCaller), "text", "image")
	path := writePNG(t, filepath.Join(t.TempDir(), "shot.png"), 3, 2, 0)
	m = pasteDraft(t, m, "Reference: "+path+"\nCompare with: ")
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyBackspace})
	m = typeText(t, m, "Z")
	m = runCommand(t, m, command, "pasting an image path started no read")
	text := "Reference: " + path + "\nCompare with: " + path[:len(path)-1] + "Z"
	assertDraftAndCaret(t, m, text+"[Image #1 3x2]", len([]rune(text))+1)
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

func TestImagePasteUsesTheModelTheThreadRuns(t *testing.T) {
	t.Parallel()
	m := newModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m, _ = drive(t, m, resultMsg{op: "levels", value: &rpc.ModelListResponse{Data: []rpc.Model{{ID: "local/qwen-test", InputModalities: []string{"text", "image"}}}}})
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{Model: "local/qwen-test", Thread: rpc.Thread{ID: "t1"}}})
	path := writePNG(t, filepath.Join(t.TempDir(), "pixel.png"), 1, 1, 0)
	m, command := drive(t, m, tea.PasteMsg{Content: path})
	m = runCommand(t, m, command, "pasting an image path started no read")
	if !strings.Contains(dockText(m), "[Image #1 1x1]") {
		t.Fatalf("image path made no chip; notice %q; dock:\n%s", m.notice, dockText(m))
	}
}
