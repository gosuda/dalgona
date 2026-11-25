//go:build gui

package composer

import (
	"bytes"
	"encoding/base64"
	"encoding/json/v2"
	"errors"
	"image"
	"image/png"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestParseEntryFollowsAsciiTriggersAndEscape(t *testing.T) {
	cases := []struct {
		text  string
		kind  entryKind
		body  string
		query string
		open  bool
	}{
		{text: "!  ls -la \t", kind: entryShell, body: "  ls -la \t"},
		{text: "//model please", kind: entryMessage, body: "/model please"},
		{text: "/etc/hosts", kind: entryMessage, body: "/etc/hosts"},
		{text: "/model", kind: entryCommand, body: "model", query: "model", open: true},
		{text: "/", kind: entryCommand, body: "", query: "", open: true},
		{text: "/model gpt", kind: entryCommand, body: "model gpt"},
		{text: "/model\u3000", kind: entryCommand, body: "model\u3000"},
		{text: "\uff01ls", kind: entryMessage, body: "\uff01ls"},
		{text: "\uff0fmodel", kind: entryMessage, body: "\uff0fmodel"},
		{text: "hello /model", kind: entryMessage, body: "hello /model"},
	}
	for _, c := range cases {
		e := parseEntry(c.text)
		query, open := e.commandQuery()
		if e.kind != c.kind || e.body != c.body || query != c.query || open != c.open {
			t.Errorf("parseEntry(%q) = %v %q query %q %v; want %v %q query %q %v", c.text, e.kind, e.body, query, open, c.kind, c.body, c.query, c.open)
		}
	}
}

func names(cs []command) []string {
	out := make([]string, len(cs))
	for i, c := range cs {
		out[i] = c.name
	}
	return out
}

func TestRankCommandsKeepsEveryRequiredCommand(t *testing.T) {
	want := []string{"model", "settings", "login", "resume", "new", "compact", "fork", "tree", "revert", "advisor", "review", "help", "remote", "runner", "mcp", "files", "terminal", "doctor", "failures"}
	if got := names(rankCommands("")); strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("empty query lists %v, want %v", got, want)
	}
}

func TestRankCommandsPutsPrefixMatchesBeforeFuzzyMatches(t *testing.T) {
	got := names(rankCommands("Re"))
	want := []string{"resume", "revert", "review", "remote", "tree", "failures", "runner"}
	if strings.Join(got, " ") != strings.Join(want, " ") {
		t.Fatalf("rank(Re) = %v, want %v", got, want)
	}
	if got := names(rankCommands("mdl")); strings.Join(got, " ") != "model" {
		t.Fatalf("rank(mdl) = %v, want [model]", got)
	}
	if got := rankCommands("zz"); len(got) != 0 {
		t.Fatalf("rank(zz) = %v, want none", names(got))
	}
}

func TestCommandPrerequisitesNameTheUnmetCondition(t *testing.T) {
	compact, _ := lookupCommand("compact")
	if got := compact.unmet(threadContext{}); got != reasonNeedsThread {
		t.Fatalf("compact without a thread: %q", got)
	}
	if got := compact.unmet(threadContext{open: true, running: true}); got != reasonNeedsIdle {
		t.Fatalf("compact while running: %q", got)
	}
	model, _ := lookupCommand("model")
	if got := model.unmet(threadContext{running: true}); got != "" {
		t.Fatalf("model is always available, got %q", got)
	}
}

func TestMentionAtStartsOnlyAtAsciiAtWordBoundary(t *testing.T) {
	cases := []struct {
		text  string
		caret int
		ok    bool
		query string
		span  textinput.Range
	}{
		{text: "see @src/ma", caret: 11, ok: true, query: "src/ma", span: textinput.Range{Start: 4, End: 11}},
		{text: "@abc def", caret: 2, ok: true, query: "abc", span: textinput.Range{Start: 0, End: 4}},
		{text: "@abc def", caret: 7},
		{text: "mail a@b", caret: 8},
		{text: "\uff20file", caret: 7},
		{text: "x\u3000@한글 y", caret: 11, ok: true, query: "한글", span: textinput.Range{Start: 4, End: 11}},
		{text: "@", caret: 1, ok: true, query: "", span: textinput.Range{Start: 0, End: 1}},
	}
	for _, c := range cases {
		m, ok := mentionAt(c.text, c.caret)
		if ok != c.ok || m.query != c.query || m.span != c.span {
			t.Errorf("mentionAt(%q, %d) = %+v %v; want %q %+v %v", c.text, c.caret, m, ok, c.query, c.span, c.ok)
		}
	}
}

func TestNormalizePasteConvertsLineEndingsAndKeepsFinalLF(t *testing.T) {
	p, err := normalizePaste("a\r\nb\rc\n")
	if err != nil || p != (pastedText{text: "a\nb\nc\n", lines: 3}) {
		t.Fatalf("normalizePaste = %+v %v", p, err)
	}
	ten := strings.Repeat("line\r\n", 10)
	p, err = normalizePaste(ten)
	if err != nil || p != (pastedText{text: strings.Repeat("line\n", 10), lines: pasteChipLines}) {
		t.Fatalf("ten lines = %+v %v", p, err)
	}
	chip := pasteChip("paste-1", p)
	if chip != (state.Attachment{ID: "paste-1", Kind: state.AttachmentText, Text: p.text, Name: "Pasted text · 10 lines"}) {
		t.Fatalf("chip = %+v", chip)
	}
	if _, err := normalizePaste("a\x00b"); !errors.Is(err, textinput.DisallowedControl) {
		t.Fatalf("NUL paste error = %v", err)
	}
}

func pngBytes(t *testing.T, w, h int) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := png.Encode(&buf, image.NewGray(image.Rect(0, 0, w, h))); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestImageChipUsesMagicBytesAndPreservesWireBytes(t *testing.T) {
	data := pngBytes(t, 3, 2)
	chip, err := imageChip("image-1", imageFile{name: "photo.gif", data: data})
	if err != nil {
		t.Fatal(err)
	}
	payload, ok := strings.CutPrefix(chip.URL, "data:image/png;base64,")
	decoded, decodeErr := base64.StdEncoding.DecodeString(payload)
	if !ok || decodeErr != nil || !bytes.Equal(decoded, data) || chip.Name != "photo.gif" || imageSize(chip) != len(data) {
		t.Fatalf("chip = %q name %q size %d", chip.URL[:min(40, len(chip.URL))], chip.Name, imageSize(chip))
	}
}

func TestImageChipRefusesInvalidFiles(t *testing.T) {
	data := pngBytes(t, 3, 2)
	if _, err := imageChip("x", imageFile{name: "a.png", data: []byte("not an image")}); !errors.Is(err, errImageType) {
		t.Fatalf("text as image: %v", err)
	}
	big := append(bytes.Clone(data), make([]byte, imageBytes)...)
	if _, err := imageChip("x", imageFile{name: "big.png", data: big}); !errors.Is(err, errImageTooLarge) {
		t.Fatalf("oversized image: %v", err)
	}
	if _, err := imageChip("x", imageFile{name: "wide.png", data: pngBytes(t, imageSideLimit+1, 1)}); !errors.Is(err, errImageSize) {
		t.Fatalf("too wide image: %v", err)
	}
	damaged := bytes.Clone(data[:12])
	if _, err := imageChip("x", imageFile{name: "cut.png", data: damaged}); err == nil {
		t.Fatal("damaged PNG accepted")
	}
}

func imageOf(size int) state.Attachment {
	return state.Attachment{Kind: state.AttachmentImage, URL: "data:image/png;base64," + base64.StdEncoding.EncodeToString(make([]byte, size))}
}

func TestAdmitImageEnforcesCountAndCombinedBytes(t *testing.T) {
	var ten []state.Attachment
	for range imageLimit {
		ten = append(ten, imageOf(1))
	}
	if err := admitImage(ten, imageOf(1)); !errors.Is(err, errImageCount) {
		t.Fatalf("eleventh image: %v", err)
	}
	two := []state.Attachment{imageOf(imageBytes), imageOf(imageBytes)}
	if err := admitImage(two[:1], two[1]); err != nil {
		t.Fatalf("exactly 10 MiB: %v", err)
	}
	if err := admitImage(two, imageOf(1)); !errors.Is(err, errImageTotal) {
		t.Fatalf("over 10 MiB: %v", err)
	}
	if got := imageSize(imageOf(4)); got != 4 {
		t.Fatalf("decoded size of padded payload = %d", got)
	}
}

func TestInputBytesMatchesSerializedInputArray(t *testing.T) {
	typed := "say \"hi\"\n\t<b>&\u2028 한글 \x01"
	named := imageOf(5)
	named.Name = "shot.png"
	chips := []state.Attachment{
		{Kind: state.AttachmentText, Text: "pasted \\ text\r\n"},
		named,
		{Kind: state.AttachmentText, Text: "second"},
	}
	items := []rpc.InputItem{
		{Type: "text", Text: typed},
		{Type: "text", Text: chips[0].Text},
		{Type: "text", Text: chips[2].Text},
		{Type: "image", URL: chips[1].URL, Name: chips[1].Name},
	}
	want, err := json.Marshal(items)
	if err != nil {
		t.Fatal(err)
	}
	if got := inputBytes(typed, chips); got != len(want) {
		t.Fatalf("inputBytes = %d, serialized %d (%s)", got, len(want), want)
	}
	if got, want := inputBytes("", nil), len("[]"); got != want {
		t.Fatalf("empty input = %d, want %d", got, want)
	}
}

func TestKeywordSpansConvertValidatedByteRanges(t *testing.T) {
	text := "한국어 test e\u0301x"
	spans, ok := KeywordSpans(text, []rpc.KeywordMatch{
		{Word: "test", ByteRange: rpc.ByteRange{Start: 10, End: 14}},
		{Word: "e", ByteRange: rpc.ByteRange{Start: 15, End: 16}},
	})
	if !ok || len(spans) != 2 {
		t.Fatalf("spans = %+v %v", spans, ok)
	}
	if spans[0].Runes != (textinput.Range{Start: 4, End: 8}) || spans[0].Paint != spans[0].Runes {
		t.Fatalf("test span = %+v", spans[0])
	}
	if spans[1].Bytes != (textinput.Range{Start: 15, End: 16}) || spans[1].Runes != (textinput.Range{Start: 9, End: 10}) || spans[1].Paint != (textinput.Range{Start: 9, End: 11}) {
		t.Fatalf("span inside a grapheme = %+v", spans[1])
	}
	if got := KeywordWords(append(spans, spans[0])); got != "Keywords: test, e" {
		t.Fatalf("words = %q", got)
	}
}

func TestKeywordSpansRejectMalformedMetadata(t *testing.T) {
	text := "한국어 test"
	bad := [][]rpc.KeywordMatch{
		{{Word: "x", ByteRange: rpc.ByteRange{Start: 1, End: 3}}},
		{{Word: "x", ByteRange: rpc.ByteRange{Start: 10, End: 15}}},
		{{Word: "x", ByteRange: rpc.ByteRange{Start: 4, End: 4}}},
		{{Word: "x", ByteRange: rpc.ByteRange{Start: -1, End: 3}}},
		{{Word: "a", ByteRange: rpc.ByteRange{Start: 10, End: 14}}, {Word: "b", ByteRange: rpc.ByteRange{Start: 0, End: 3}}},
	}
	for _, matches := range bad {
		if spans, ok := KeywordSpans(text, matches); ok || spans != nil {
			t.Errorf("KeywordSpans(%+v) = %+v %v, want rejected", matches, spans, ok)
		}
	}
}

func TestInterruptGuardNeedsTwoEscapesWithinTwoSeconds(t *testing.T) {
	var g interruptGuard
	start := time.Unix(1_800_000_000, 0)
	if g.escape(start, false) || !g.armed(start.Add(time.Second)) {
		t.Fatal("first Escape must only arm the guard")
	}
	if !g.escape(start.Add(1900*time.Millisecond), false) || g.armed(start.Add(1950*time.Millisecond)) {
		t.Fatal("second Escape inside the guard must interrupt and disarm")
	}
	if g.escape(start.Add(3*time.Second), false) || g.escape(start.Add(5100*time.Millisecond), false) {
		t.Fatal("an Escape after the guard expired must re-arm, not interrupt")
	}
	var waiting interruptGuard
	if !waiting.escape(start, true) {
		t.Fatal("Escape during a rate-limit wait must interrupt at once")
	}
}

func TestRecallBrowsesHistoryAndRestoresDraft(t *testing.T) {
	history := recallHistory([]string{"a", "a", "b", ""}, []string{"b", "c"})
	if strings.Join(history, ",") != "a,b,c" {
		t.Fatalf("history = %q", history)
	}
	r := recall{browsing: -1}
	draft := textinput.Draft{Text: "draft", Selection: textinput.Range{Start: 2, End: 2}}
	requireOlderPrompts(t, &r, history, draft)
	for _, want := range []string{"b", "c"} {
		if got, ok := r.newer(history); !ok || got.Text != want {
			t.Fatalf("newer = %+v %v, want %q", got, ok, want)
		}
	}
	if got, ok := r.newer(history); !ok || got != draft || !r.idle() {
		t.Fatalf("newer past the newest = %+v %v, want the saved draft", got, ok)
	}
}

func requireOlderPrompts(t *testing.T, r *recall, history []string, draft textinput.Draft) {
	t.Helper()
	for _, want := range []string{"c", "b", "a"} {
		got, ok := r.older(history, draft)
		if !ok || got.Text != want || got.Selection != (textinput.Range{}) {
			t.Fatalf("older = %+v %v, want %q at start", got, ok, want)
		}
	}
	if _, ok := r.older(history, draft); ok {
		t.Fatal("older past the oldest message must stay")
	}
}

func TestShellTextFenceExceedsEveryBacktickRun(t *testing.T) {
	output, lines := shellOutput("a ```` b\nc\n\n")
	if output != "a ```` b\nc" || lines != 2 {
		t.Fatalf("shellOutput = %q %d", output, lines)
	}
	got := shellText("cat x", 1, output, false)
	want := "Output of `cat x` (exit 1):\n`````\na ```` b\nc\n`````"
	if got != want {
		t.Fatalf("shellText = %q, want %q", got, want)
	}
	if got := shellText("true", 0, "", true); got != "Output of `true` (exit 0, truncated at 64 KiB):\n```\n\n```" {
		t.Fatalf("empty truncated = %q", got)
	}
	if got := shellChipName(2, 3, 1, false); got != "[Shell #2 exit 3, 1 line]" {
		t.Fatalf("chip name = %q", got)
	}
	if got := shellChipName(4, 0, 12, true); got != "[Shell #4 exit 0, 12 lines] · output truncated at 64 KiB" {
		t.Fatalf("truncated chip name = %q", got)
	}
}
