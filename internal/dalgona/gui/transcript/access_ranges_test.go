//go:build gui

package transcript

import (
	"errors"
	"image"
	"strings"
	"testing"

	"gioui.org/f32"
	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestAccessibilityCrossObjectRangeResumesWhenTextArrives(t *testing.T) {
	withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
		last, call := accessStudyRange(t, h)
		first := h.ax.Find(t, "Source bytes 1 to 16384", platform.RoleDocument)
		queryStudyRange(t, h, first, platform.ErrAccessibilityBusy)
		h.frame()
		h.frame()
		queryStudyRange(t, h, first, nil)
		gate := holdAccessRead(h, call)
		snap := h.ax.Tree.Snapshot()
		span := nativeRange(first, accessPageBytes-8, last, 9)
		for range 2 {
			if text, err := h.ax.Tree.QueryTextRange(snap, span); text != "" || !errors.Is(err, platform.ErrAccessibilityBusy) {
				close(gate)
				t.Fatalf("range returned %q, %v while one object was uncached", text, err)
			}
		}
		h.frame()
		close(gate)
		h.frame()
		h.frame()
		ready := h.ax.Tree.Snapshot()
		text, err := h.ax.Tree.QueryTextRange(ready, span)
		want := string(h.d.contents[call.content][call.offset-8 : call.offset+9])
		if err != nil || text != want || ready.Focus() != snap.Focus() || rangeReads(h, call) != 1 {
			t.Fatalf("range = %q, %v; want %q, unchanged focus, and one read", text, err, want)
		}
	})
}

func TestAccessibilityCrossObjectSelectionCopiesLikeKeyboard(t *testing.T) {
	text := "## Start 😀\n\n" + strings.Repeat("A **middle** paragraph.\n\n", 24) + "## End 한글"
	withTranscript(t, assistantTurn(text), func(t *testing.T, h *harness) {
		h.size.Y = 128
		h.ax = accesstest.New()
		h.frame()
		h.tap(f32.Pt(10, 50), 0)
		h.keyPress("A", key.ModShortcut)
		keyboard := copiedTranscript(t, h)
		h.keyPress(key.NameEscape, 0)
		first := h.ax.Find(t, "Start 😀", platform.RoleHeading)
		last := h.ax.Find(t, "End 한글", platform.RoleHeading)
		if !first.Bounds.Empty() || last.Bounds.Empty() {
			t.Fatalf("test needs an offscreen start and a visible end: %v, %v", first.Bounds, last.Bounds)
		}
		span := nativeRange(first, 0, last, len(last.Value))
		snap := h.ax.Tree.Snapshot()
		if err := h.ax.Tree.SelectTextRange(snap, span); err != nil {
			t.Fatal(err)
		}
		h.frame()
		if selected := copiedTranscript(t, h); selected != keyboard {
			t.Fatalf("native selection copied %q, keyboard copied %q", selected, keyboard)
		}
		if readable, err := h.ax.Tree.QueryTextRange(snap, span); err != nil || readable != keyboard {
			t.Fatalf("native range read %q, %v, keyboard copied %q", readable, err, keyboard)
		}
	})
}

func nativeRange(first platform.SemanticNode, start int, last platform.SemanticNode, end int) platform.TextRange {
	return platform.TextRange{Start: platform.TextPoint{Node: first.ID, Byte: start}, End: platform.TextPoint{Node: last.ID, Byte: end}}
}

func copiedTranscript(t *testing.T, h *harness) string {
	t.Helper()
	h.keyPress("C", key.ModShortcut)
	mime, text, ok := h.router.WriteClipboard()
	if !ok || mime != "application/text" || len(text) == 0 {
		t.Fatalf("no transcript text was copied: %q, %s, %v", text, mime, ok)
	}
	return string(text)
}

func TestAccessibilityFigureAndImageControlsPublishHitBounds(t *testing.T) {
	withTranscript(t, semanticTurn, func(t *testing.T, h *harness) {
		h.size = image.Pt(800, 900)
		h.ax = accesstest.New()
		installFigureOutcome(t, h)
		for _, name := range []string{"Show source", "Hide preview for map.png"} {
			n := h.ax.Find(t, name, platform.RoleButton)
			control, ok := h.button(name)
			if !ok || n.Bounds.Empty() || n.Bounds != control.Desc.Bounds {
				t.Errorf("%s native bounds = %v, drawn control = %v (present %t)", name, n.Bounds, control.Desc.Bounds, ok)
			}
		}
	})
}
