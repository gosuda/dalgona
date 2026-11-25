//go:build gui

package transcript

import (
	json "encoding/json/v2"
	"image"
	"strings"
	"testing"
	"time"

	"gioui.org/f32"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/semantic"
	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

func diffNavigationTurn(t *testing.T, before, after, suffix string) (func(*daemon) []rpc.HistoryTurn, string) {
	t.Helper()
	patch := "diff --git a/a.go b/a.go\r\n--- a/a.go\r\n+++ b/a.go\r\n@@ -7,26 +9,26 @@ first\r\n-" + before + "\r\n+" + after + "\r\n" +
		strings.Repeat(" context\r\n", 25) + suffix
	changes := []map[string]any{
		{"path": "src/a.go", "kind": map[string]string{"type": "update"}, "diff": patch},
		{"path": "src/b.go", "kind": map[string]string{"type": "add"}, "diff": "--- /dev/null\n+++ b/src/b.go\n@@ -0,0 +1 @@\n+new file\n"},
		{"path": "src/omitted.go", "kind": map[string]string{"type": "update"}, "diffOmitted": true},
	}
	raw, err := json.Marshal(changes)
	if err != nil {
		t.Fatal(err)
	}
	return func(d *daemon) []rpc.HistoryTurn {
		return []rpc.HistoryTurn{frozenTurn("diff-turn", rpc.StatusCompleted,
			d.header("diff-item", rpc.KindFileChange, map[string]any{"status": rpc.StatusCompleted}, fieldData{name: "changes", data: string(raw), json: true}),
		)}
	}, patch
}

func TestTranscriptDiffActionsNavigateAndCopyOriginalPatch(t *testing.T) {
	turn, patch := diffNavigationTurn(t, "old", "new", "@@ -100 +102 @@ second\r\n-removed\r\n+changed\r\n")
	withTranscript(t, turn, func(t *testing.T, h *harness) {
		h.size = image.Pt(800, 300)
		h.frame()
		startDiff(t, h)
		clickDiff(t, h, "Copy patch")
		assertActionClipboard(t, h, patch)
		clickDiff(t, h, "Next hunk")
		assertDiffVisible(t, h, "@@ -7,26 +9,26 @@ first")
		clickDiff(t, h, "Next hunk")
		assertDiffVisible(t, h, "@@ -100 +102 @@ second")
		assertDiffVisible(t, h, "Old line 100")
		assertDiffVisible(t, h, "New line 102")
		startDiff(t, h)
		clickDiff(t, h, "Next file")
		assertDiffVisible(t, h, "diff src/b.go · Added")
		clickDiff(t, h, "Open file")
		want := shell.Route{Panel: "files", Thread: h.key, Context: "src/b.go"}
		if len(h.routes) != 1 || h.routes[0] != want {
			t.Fatalf("file action routed to %+v, want %+v", h.routes, want)
		}
		clickDiff(t, h, "Next file")
		assertDiffVisible(t, h, "Diff not shown.")
		clickDiff(t, h, "Copy patch")
		if _, _, ok := h.router.WriteClipboard(); ok || !strings.Contains(h.tr.notice, "complete patch") {
			t.Fatalf("omitted patch copied or did not report failure: %q", h.tr.notice)
		}
	})
}

func TestTranscriptSplitDiffPairsSidesScrollsEachAndKeepsFollowingFiles(t *testing.T) {
	left := "diff payload " + strings.Repeat("old side ", 45)
	right := strings.Repeat("new side ", 45)
	turn, patch := diffNavigationTurn(t, left, right, "")
	withTranscript(t, turn, func(t *testing.T, h *harness) {
		startDiff(t, h)
		clickDiff(t, h, "Side-by-side")
		h.frame()
		assertDiffVisible(t, h, "Unified")
		assertSplitSections(t, h)
		l, okL := visibleLabelBounds(h, "Old line 7")
		r, okR := visibleLabelBounds(h, "New line 9")
		if !okL || !okR || l.Min.Y != r.Min.Y || l.Max.X >= r.Min.X {
			t.Fatalf("changed lines did not align across columns: left %v (%t), right %v (%t)", l, okL, r, okR)
		}
		assertSplitScroll(t, h, l, r)
		startDiff(t, h)
		clickDiff(t, h, "Copy patch")
		assertActionClipboard(t, h, patch)
		clickDiff(t, h, "Next file")
		assertDiffVisible(t, h, "diff src/b.go · Added")
		assertDiffVisible(t, h, "Collapse file")
		clickDiff(t, h, "Next hunk")
		assertDiffVisible(t, h, "@@ -0,0 +1 @@")
	})
}

func assertSplitSections(t *testing.T, h *harness) {
	t.Helper()
	want := [...]string{"diff src/a.go · Changed\n", "diff src/b.go · Added\n", "diff src/omitted.go · Changed\n"}
	doc, ok := h.tr.panes[h.key].scope.Loaded(0)
	if !ok || doc.Len() != len(want) {
		t.Fatal("split projection did not preserve one foldable section per file across paired rows and Git metadata")
	}
	for i, heading := range want {
		if text := doc.Block(i).Projection.Text; !strings.HasPrefix(text, heading) {
			t.Fatalf("split file section %d starts %q, want %q", i, strings.SplitN(text, "\n", 2)[0], heading)
		}
	}
}

func assertSplitScroll(t *testing.T, h *harness, left, right image.Rectangle) {
	t.Helper()
	p := h.tr.panes[h.key]
	at := func(r image.Rectangle) f32.Point {
		return f32.Pt(float32(r.Max.X+64), float32(r.Min.Y+r.Dy()/2))
	}
	l, r := at(left), at(right)
	beforeL, okL := p.hit(l)
	beforeR, okR := p.hit(r)
	if !okL || !okR || beforeL.Offset >= beforeR.Offset {
		t.Fatalf("could not select both shaped sides: %v/%v and %v/%v", beforeL, okL, beforeR, okR)
	}
	h.router.Queue(pointer.Event{Kind: pointer.Scroll, Source: pointer.Mouse, Position: l, Scroll: f32.Pt(110, 0)})
	h.frame()
	afterL, _ := p.hit(l)
	afterR, _ := p.hit(r)
	if afterL.Offset <= beforeL.Offset || afterR != beforeR {
		t.Fatalf("scrolling old side moved old %d→%d, new %d→%d; new must stay fixed", beforeL.Offset, afterL.Offset, beforeR.Offset, afterR.Offset)
	}
}

func startDiff(t *testing.T, h *harness) {
	t.Helper()
	h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
	h.keyPress(key.NameHome, 0)
	h.frame()
	assertDiffVisible(t, h, "diff src/a.go · Changed")
}

func clickDiff(t *testing.T, h *harness, name string) {
	t.Helper()
	for _, n := range h.nodes {
		d := n.Desc
		if d.Label != name || d.Class != semantic.Button || d.Disabled || d.Bounds.Dy() < 12 || d.Bounds.Min.Y < 0 || d.Bounds.Max.Y > h.size.Y {
			continue
		}
		p := f32.Pt(float32(d.Bounds.Min.X+d.Bounds.Dx()/2), float32(d.Bounds.Min.Y+d.Bounds.Dy()/2))
		at := time.Duration(h.now.UnixMilli()) * time.Millisecond
		h.router.Queue(pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: p, Time: at},
			pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Position: p, Time: at})
		h.frame()
		h.frame()
		return
	}
	t.Fatalf("no visible diff button %q among %q", name, h.labels())
}

func visibleLabelBounds(h *harness, label string) (image.Rectangle, bool) {
	viewport := image.Rectangle{Max: h.size}
	for _, n := range h.nodes {
		b := n.Desc.Bounds
		if n.Desc.Label == label && !b.Empty() && b.In(viewport) {
			return b, true
		}
	}
	return image.Rectangle{}, false
}

func assertDiffVisible(t *testing.T, h *harness, label string) {
	t.Helper()
	if _, ok := visibleLabelBounds(h, label); !ok {
		t.Fatalf("diff label %q is not visible among %q", label, h.labels())
	}
}

func TestProjectedDiffCannotBeUsedForMarkdown(t *testing.T) {
	id := document.Identity{Endpoint: "local", Thread: "t1", Turn: "u1", Item: "a"}
	if _, err := document.New(document.Header{Identity: id, Format: document.FormatMarkdown, PatchProjection: true}, "text"); err == nil {
		t.Fatal("Markdown accepted diff projection boundaries")
	}
}
