//go:build gui

package transcript

import (
	"fmt"
	"image"
	"slices"
	"strings"
	"testing"

	"gioui.org/f32"
	"gioui.org/io/key"
	"gioui.org/io/pointer"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
)

func TestTranscriptEnterActivatesOnlyTheAllowedCaretLink(t *testing.T) {
	const target = "https://example.org/guide"
	const source = "Read [guide](" + target + ") or [unsafe](javascript:alert(1))."
	var opened []string
	deps := Deps{OpenLink: func(url string) error { opened = append(opened, url); return nil }}
	withTranscriptDeps(t, "", deps, assistantTurn(source), func(t *testing.T, h *harness) {
		h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
		for range len("Read g") + 1 {
			h.keyPress(key.NameRightArrow, 0)
		}
		h.keyPress(key.NameReturn, 0)
		h.frame()
		if !slices.Equal(opened, []string{target}) {
			t.Fatalf("Enter on guide opened %q, want exactly %q", opened, target)
		}
		for range len("uide or u") {
			h.keyPress(key.NameRightArrow, 0)
		}
		h.keyPress(key.NameReturn, 0)
		h.frame()
		if !slices.Equal(opened, []string{target}) || !strings.Contains(h.tr.notice, "javascript:") {
			t.Fatalf("Enter on unsafe link: opened %q, notice %q", opened, h.tr.notice)
		}
	})
}

func TestTranscriptShiftDownShapesUnmountedRowsAndRevealsTheCaret(t *testing.T) {
	source := "anchor\n\n" + strings.TrimSpace(strings.Repeat("wrapword ", 100)) + "\n\n" + strings.Repeat("spacer\n\n", 60)
	withTranscript(t, assistantTurn(source), func(t *testing.T, h *harness) {
		h.size = image.Pt(280, 150)
		h.frame()
		h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
		h.keyPress(key.NameRightArrow, 0)
		p := h.tr.panes[h.key]
		anchor := p.sel.Selection.Anchor
		h.keyPress(key.NameEnd, 0)
		if h.has("anchor") {
			t.Fatal("setup failed to scroll the start outside mounted rows")
		}
		h.keyPress(key.NameDownArrow, key.ModShift)
		first := p.sel.Selection.Focus
		if first.Offset != len("anchor\n\n") {
			t.Fatalf("Shift+Down across unmounted blocks reached source offset %d, want %d", first.Offset, len("anchor\n\n"))
		}
		for range 4 {
			prev := p.sel.Selection.Focus.Offset
			h.keyPress(key.NameDownArrow, key.ModShift)
			if p.sel.Selection.Focus.Offset <= prev {
				t.Fatalf("Shift+Down stopped advancing at source offset %d", prev)
			}
		}
		if p.sel.Selection.Anchor != anchor {
			t.Fatalf("Shift+Down moved selection anchor: got %+v, want %+v", p.sel.Selection.Anchor, anchor)
		}
		assertVisibleCaret(t, h, p.sel.Selection.Focus)
	})
}

func assertVisibleCaret(t *testing.T, h *harness, pos document.Position) {
	t.Helper()
	p := h.tr.panes[h.key]
	index, ok := p.vp.Locate(p.scope, pos)
	if !ok {
		t.Fatal("the focus does not resolve to a transcript row")
	}
	for _, pl := range p.placed {
		if pl.index != index || pl.layout == nil {
			continue
		}
		s, ok := p.resolve(pos)
		if !ok {
			t.Fatal("the focus does not resolve to shaped text")
		}
		caret, height := pl.layout.Caret(s.text, pos.Affinity)
		y := float32(pl.y+pl.text.Y) + caret.Y
		if y < -1 || y+height > float32(h.size.Y+1) {
			t.Fatalf("caret at [%f,%f] is outside viewport height %d", y, y+height, h.size.Y)
		}
		return
	}
	t.Fatal("the focus remains unmounted after keyboard movement")
}

func TestTranscriptDragAutoscrollContinuesWithoutPointerMotion(t *testing.T) {
	var lines strings.Builder
	for i := range 80 {
		fmt.Fprintf(&lines, "row %03d\n\n", i)
	}
	withTranscript(t, assistantTurn(lines.String()), func(t *testing.T, h *harness) {
		h.size = image.Pt(480, 180)
		h.frame()
		h.router.Source().Execute(key.FocusCmd{Tag: h.tr.FocusTag()})
		h.keyPress(key.NameHome, 0)
		at := h.textPoint("row 000")
		h.router.Queue(pointer.Event{Kind: pointer.Press, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: at})
		h.frame()
		p := h.tr.panes[h.key]
		anchor := p.sel.Selection.Anchor
		outside := f32.Pt(at.X, float32(h.size.Y+400))
		h.router.Queue(pointer.Event{Kind: pointer.Move, Source: pointer.Mouse, Buttons: pointer.ButtonPrimary, Position: outside})
		h.frame()
		before, focusBefore, started := p.vp.Offset(), p.sel.Selection.Focus.Offset, h.now
		for range 10 {
			h.frame()
		}
		advanced := p.vp.Offset() - before
		maxTravel := document.MaxDragScrollSpeed * h.now.Sub(started).Seconds()
		if advanced <= 0 || advanced > maxTravel+1 || p.sel.Selection.Focus.Offset <= focusBefore || p.sel.Selection.Anchor != anchor {
			t.Fatalf("stationary drag moved %.1f px (bound %.1f), focus %d→%d, anchor %+v→%+v", advanced, maxTravel, focusBefore, p.sel.Selection.Focus.Offset, anchor, p.sel.Selection.Anchor)
		}
		assertDragStopped(t, h, outside)
	})
}

func assertDragStopped(t *testing.T, h *harness, outside f32.Point) {
	t.Helper()
	p := h.tr.panes[h.key]
	h.router.Queue(pointer.Event{Kind: pointer.Release, Source: pointer.Mouse, Position: outside})
	h.frame()
	stopped, focus := p.vp.Offset(), p.sel.Selection.Focus
	for range 4 {
		h.frame()
	}
	if p.vp.Offset() != stopped || p.sel.Selection.Focus != focus {
		t.Fatal("drag kept scrolling or selecting after release")
	}
}
