//go:build gui

package transcript

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"gioui.org/f32"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	gmath "github.com/gosuda/dalgona/internal/dalgona/gui/figure/math"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func (h *harness) placedText(text string) placed {
	h.t.Helper()
	for _, pl := range h.tr.panes[h.key].placed {
		if pl.layout == nil {
			continue
		}
		at := strings.Index(pl.layout.Text, text)
		if at >= pl.layout.Row.Start && at < pl.layout.Row.End {
			return pl
		}
	}
	h.t.Fatalf("no laid-out row shows %q among %q", text, h.labels())
	return placed{}
}

func (h *harness) textPoint(text string) f32.Point {
	h.t.Helper()
	pl := h.placedText(text)
	caret, height := pl.layout.Caret(strings.Index(pl.layout.Text, text)+1, document.After)
	return f32.Pt(float32(pl.text.X)+caret.X, float32(pl.y+pl.text.Y)+caret.Y+height/2)
}

func (h *harness) tap(at f32.Point, mods key.Modifiers) {
	h.router.Queue(
		pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at, Modifiers: mods},
		pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Position: at, Modifiers: mods},
	)
	h.frame()
}

func (h *harness) focusBlock() int {
	h.t.Helper()
	p := h.tr.panes[h.key]
	s, ok := p.resolve(p.sel.Selection.Focus)
	if !ok || !p.sel.Selected {
		h.t.Fatalf("the selection focus %+v does not resolve", p.sel.Selection.Focus)
	}
	return s.block
}

func assistantTurn(text string) func(d *daemon) []rpc.HistoryTurn {
	return func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted, d.header("a1", rpc.KindAgent, nil, fieldData{name: "text", data: text}))}
	}
}

func foldedCode() string {
	var text strings.Builder
	text.WriteString("Intro line\n\n```go\n")
	for i := 1; i <= 12; i++ {
		text.WriteString("code " + strconv.Itoa(i) + "\n")
	}
	text.WriteString("```\n\nAfter text")
	return text.String()
}

func TestFoldedCodeSurvivesNewRowsAndLineMovementSkipsIt(t *testing.T) {
	withTranscript(t, assistantTurn(foldedCode()), func(t *testing.T, h *harness) {
		h.tap(h.textPoint("Intro"), 0)
		h.keyPress(key.NameDownArrow, key.ModShift)
		h.keyPress(key.NameDownArrow, key.ModShift)
		if b := h.focusBlock(); b != 1 {
			t.Fatalf("two lines down from the intro reach block %d, want the unfolded code block", b)
		}
		h.click("Collapse code")
		h.settle()
		h.expect("code 1", "… 11 more lines folded", "Expand code")
		if h.has("code 2") {
			t.Fatal("a folded code block still shows its second line")
		}
		h.d.event("t1", "turn/started", map[string]any{"turn": map[string]any{"id": "u2", "status": "inProgress"}})
		h.settle()
		h.expect(outcomeWorking, "… 11 more lines folded")
		if h.has("code 2") {
			t.Fatal("a new transcript row unfolded the code block")
		}
		h.tap(h.textPoint("Intro"), 0)
		h.keyPress(key.NameDownArrow, key.ModShift)
		h.keyPress(key.NameDownArrow, key.ModShift)
		if b := h.focusBlock(); b != 2 {
			t.Fatalf("two lines down from the intro reach block %d, want the paragraph after the fold", b)
		}
	})
}

func TestRebuildPreservesTheScrollAnchorInFoldedCode(t *testing.T) {
	withTranscript(t, assistantTurn(foldedCode()), func(t *testing.T, h *harness) {
		h.click("Collapse code")
		h.settle()
		p := h.tr.panes[h.key]
		doc := p.rows[identity(h.key, "u1", "a1")].doc
		fold := doc.Folds()[0]
		row, ok := p.vp.Locate(p.scope, fold.Start)
		if !ok || !p.scope.Folded(fold.Key) {
			t.Fatal("the folded code has no visible start")
		}
		pos := fold.Start
		pos.Offset++
		want := p.vp.Capture(row, pos, 3)
		p.sel.Anchor, p.sel.Anchored = want, true
		p.rebuild(p.order)
		if !p.sel.Anchored || p.sel.Anchor != want || !p.scope.Folded(fold.Key) {
			t.Fatalf("rebuild left anchor %+v, anchored %t, folded %t; want anchor %+v and fold unchanged",
				p.sel.Anchor, p.sel.Anchored, p.scope.Folded(fold.Key), want)
		}
	})
}

func TestTripleClickSelectsOneVisualLine(t *testing.T) {
	words := make([]string, 0, 80)
	for i := range 80 {
		words = append(words, fmt.Sprintf("word%02d", i))
	}
	paragraph := strings.Join(words, " ")
	withTranscript(t, assistantTurn(paragraph), func(t *testing.T, h *harness) {
		at := h.textPoint("word00")
		for range 3 {
			h.tap(at, 0)
		}
		h.keyPress("C", key.ModShortcut)
		_, content, ok := h.router.WriteClipboard()
		got := string(content)
		if !ok || !strings.Contains(strings.TrimSpace(got), " ") || len(got) >= len(paragraph) || !strings.HasPrefix(paragraph, got) {
			t.Fatalf("triple click copied %q from %q", got, paragraph)
		}
		if rest := paragraph[len(got):]; !strings.HasSuffix(got, " ") && !strings.HasPrefix(rest, " ") {
			t.Fatalf("the selected line %q ends inside a word", got)
		}
	})
}

func TestLinksOpenThroughTheInjectedOpenerOnlyForAllowedSchemes(t *testing.T) {
	var opened []string
	deps := Deps{OpenLink: func(u string) error {
		opened = append(opened, u)
		return nil
	}}
	text := "Read [the docs](https://example.com/guide) or [this one](javascript:alert(1)) now."
	withTranscriptDeps(t, "", deps, assistantTurn(text), func(t *testing.T, h *harness) {
		h.tap(h.textPoint("Read"), 0)
		h.frame()
		if len(opened) != 0 {
			t.Fatalf("a click on plain text opened %v", opened)
		}
		h.tap(h.textPoint("the docs"), 0)
		h.frame()
		if !slices.Equal(opened, []string{"https://example.com/guide"}) || h.tr.notice != "Opened https://example.com/guide" {
			t.Fatalf("the https link opened %v with notice %q", opened, h.tr.notice)
		}
		h.tap(h.textPoint("this one"), 0)
		h.frame()
		if len(opened) != 1 || !strings.HasPrefix(h.tr.notice, "The link did not open: ") || !strings.Contains(h.tr.notice, "does not use https, http, or mailto") {
			t.Fatalf("the javascript link opened %v with notice %q", opened, h.tr.notice)
		}
	})
}

func mathDeps(t *testing.T) Deps {
	t.Helper()
	fonts := figureFonts()
	font, err := gmath.ParseFont(fonts.Math)
	if err != nil {
		t.Fatal(err)
	}
	text, err := gmath.NewTextFont(fonts.Text...)
	if err != nil {
		t.Fatal(err)
	}
	return Deps{Math: font, MathText: text}
}

func near(a, b float32) bool {
	return a-b < 0.01 && b-a < 0.01
}

func TestInlineFormulaReservesItsMeasuredBoxAndTheCacheKeyTracksIt(t *testing.T) {
	deps := mathDeps(t)
	formula, err := gmath.Parse(`e^{i\pi}+1=0`)
	if err != nil {
		t.Fatal(err)
	}
	size := float32(unit.Metric{PxPerSp: 1}.Sp(unit.Sp(theme.SizeBody)))
	want, err := deps.Math.Layout(formula, gmath.Options{Size: size, Text: deps.MathText})
	if err != nil {
		t.Fatal(err)
	}
	withTranscriptDeps(t, "", deps, assistantTurn(`Euler showed $e^{i\pi}+1=0$ holds.`), func(t *testing.T, h *harness) {
		objects := h.placedText("Euler").layout.Objects()
		if len(objects) != 1 {
			t.Fatalf("the row reserves %d formula boxes", len(objects))
		}
		box := objects[0]
		if !near(box.Rect.X1-box.Rect.X0, want.Width) || !near(box.Baseline-box.Rect.Y0, want.Ascent) || !near(box.Rect.Y1-box.Baseline, want.Descent) {
			t.Fatalf("formula box %+v, want width %v ascent %v descent %v", box, want.Width, want.Ascent, want.Descent)
		}
		h.tr.deps.Math = nil
		h.frame()
		if n := len(h.placedText("Euler").layout.Objects()); n != 0 {
			t.Fatalf("without the math font the row kept %d cached formula boxes", n)
		}
		h.tr.deps.Math = deps.Math
		h.frame()
		if n := len(h.placedText("Euler").layout.Objects()); n != 1 {
			t.Fatalf("with the math font again the row reserves %d formula boxes", n)
		}
	})
}

func spillTurn(d *daemon) []rpc.HistoryTurn {
	var full strings.Builder
	for i := 1; i <= 30000; i++ {
		fmt.Fprintf(&full, "row %05d\n", i)
	}
	preview := "row 00001\n[output truncated: 300000 bytes total; full output: blobs/secret; page it with read offset/limit]"
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
		d.header("c1", rpc.KindCommand, map[string]any{"status": rpc.StatusCompleted, "exitCode": 0},
			fieldData{name: "command", data: "seq"}, fieldData{name: "aggregatedOutput", data: preview}, fieldData{name: "outputSpill", data: full.String()}),
	)}
}

func TestSpilledOutputReadsItsReferenceAndCountsAllLines(t *testing.T) {
	withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
		h.settleUntil("Showing the last 10 of 30000 lines.")
		h.expect("row 30000", "row 29991")
		if h.has("row 29990") || h.has("row 00001") {
			t.Fatal("the spilled row shows more than the last ten lines of the spill")
		}
		for _, l := range h.labels() {
			if strings.Contains(l, "blobs/") {
				t.Fatalf("the row exposes the raw spill path in %q", l)
			}
		}
		h.keyPress("O", key.ModShortcut)
		h.settle()
		if f := h.tr.review.fields[1]; f.ref.Field != fieldSpill || f.total != 300000 {
			t.Fatalf("the viewer pages field %q of %d bytes, want the whole spill", f.ref.Field, f.total)
		}
	})
}

func pngOf(t *testing.T) []byte {
	t.Helper()
	img := image.NewNRGBA(image.Rect(0, 0, 3, 2))
	for x := range 3 {
		img.SetNRGBA(x, 1, color.NRGBA{R: 200, A: 255})
	}
	var out bytes.Buffer
	if err := png.Encode(&out, img); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func TestUserImagePreviewUsesTheVerifiedBytesAndTheSharedPixelCache(t *testing.T) {
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	woke := make(chan struct{}, 64)
	deps := Deps{Figures: figure.NewHub(sup, func() {
		select {
		case woke <- struct{}{}:
		default:
		}
	})}
	data := pngOf(t)
	turns := func(d *daemon) []rpc.HistoryTurn {
		d.images["i1#0"] = data
		return commandTurn(d)[:1]
	}
	withTranscriptLive(t, deps, turns, func(t *testing.T, h *harness) {
		select {
		case <-woke:
		case <-time.After(30 * time.Second):
			t.Fatal("the renderer child returned no image outcome")
		}
		h.settle()
		checkCachedUserPreview(t, h, sup.Pixels(), data)
		checkImageSourceOwnerRelease(t, h, sup.Pixels())
	})
}

func checkCachedUserPreview(t *testing.T, h *harness, pixels *figure.PixelCache, data []byte) {
	t.Helper()
	h.expect("Image 1 · image/png · 2.0 KiB · 3 × 2 pixels", "Image 1")
	img := h.tr.panes[h.key].rows[identity(h.key, "u1", "i1")].images[0]
	if img.preview.Image != figure.ImageDigest(data) || img.picture == nil || !img.picture.Live() || !bytes.Equal(img.data, data) {
		t.Fatalf("the image preview is %+v with picture present %v and %d resident bytes", img.preview, img.picture != nil, len(img.data))
	}
	if _, ok := pixels.Preview(img.preview); !ok {
		t.Fatal("the preview tiles are not in the shared pixel cache")
	}
	if reads := h.d.readsOf("image:i1"); !slices.Equal(reads, []uint64{0}) {
		t.Fatalf("the image was read at %v, want one page from 0", reads)
	}
}

func checkImageSourceOwnerRelease(t *testing.T, h *harness, pixels *figure.PixelCache) {
	t.Helper()
	tr := h.tr
	img := tr.panes[h.key].rows[identity(h.key, "u1", "i1")].images[0]
	encoded := cap(img.data)
	before := chargedImageFixtureSource(tr)
	release, ok := tr.leaseImageSource(img.data)
	if !ok {
		t.Fatal("the window could not lease its retained encoded image")
	}
	func() {
		defer release()
		if got := chargedImageFixtureSource(tr); got != before || got < encoded {
			t.Fatalf("image lease changed or omitted the source charge: %d -> %d, encoded %d", before, got, encoded)
		}
		tr.Close()
		if img.data != nil || pixels.Used() != 0 {
			t.Fatalf("row close left %d encoded bytes or %d preview pixel bytes", cap(img.data), pixels.Used())
		}
		if got := chargedImageFixtureSource(tr); got != encoded {
			t.Fatalf("row close retained %d source bytes; only the %d-byte in-flight lease remains", got, encoded)
		}
	}()
	if got := chargedImageFixtureSource(tr); got != 0 {
		t.Fatalf("released image loan retained %d source bytes", got)
	}
}

func chargedImageFixtureSource(tr *Transcript) int {
	tr.loader.holdMu.Lock()
	defer tr.loader.holdMu.Unlock()
	return tr.presentedSourceBytes()
}

func TestUserImageRejectsBytesThatMissTheirDigest(t *testing.T) {
	data := []byte("stored image bytes")
	sum := sha256.Sum256(data)
	digest := hex.EncodeToString(sum[:])
	total := uint64(len(data))
	split := &userImage{}
	split.accept(state.ImagePage{Data: data[:5], Next: 5, Total: total, Digest: digest})
	if split.loaded || split.err != nil {
		t.Fatalf("the first page left loaded %v, error %v", split.loaded, split.err)
	}
	split.accept(state.ImagePage{Data: data[5:], Next: total, Total: total, Digest: digest})
	if !split.loaded || !bytes.Equal(split.data, data) {
		t.Fatalf("the complete image is loaded %v with %q", split.loaded, split.data)
	}
	bad := &userImage{}
	bad.accept(state.ImagePage{Data: data, Next: total, Total: total, Digest: strings.Repeat("0", 64)})
	if bad.loaded || !errors.Is(bad.err, errImageDigest) {
		t.Fatalf("a wrong digest left loaded %v, error %v", bad.loaded, bad.err)
	}
}

func questionTurn(d *daemon) []rpc.HistoryTurn {
	details := []byte(`{"threadId":"t1","turnId":"u1","itemId":"q1","questions":[]}`)
	d.contents["req-1"] = details
	sum := sha256.Sum256(details)
	d.pending = []rpc.PendingRequest{{RequestID: "req-1", Method: "item/tool/requestUserInput", Details: rpc.ContentSummary{Total: rpc.FormatDecimal(uint64(len(details))), Digest: hex.EncodeToString(sum[:])}}}
	return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusInProgress, d.header("q1", rpc.KindDynamic, map[string]any{"tool": "question", "status": rpc.StatusInProgress}))}
}

func TestTimedOutRequestLabelsItsItemFromTheResolutions(t *testing.T) {
	withTranscript(t, questionTurn, func(t *testing.T, h *harness) {
		if h.has("Timed out") {
			t.Fatal("the pending question already reads Timed out")
		}
		h.d.event("t1", "serverRequest/resolved", map[string]any{"threadId": "t1", "requestId": "req-1", "resolution": "timedOut"})
		h.settle()
		h.expect("Timed out")
	})
}
