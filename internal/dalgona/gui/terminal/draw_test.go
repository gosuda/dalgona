//go:build gui

package terminal

import (
	"image"
	"testing"

	"gioui.org/font"
	"gioui.org/font/gofont"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"
	"gioui.org/unit"
	"golang.org/x/image/math/fixed"
)

func TestPainterSizesTheGridFromTheMonospaceAdvance(t *testing.T) {
	shaper := text.NewShaper(text.NoSystemFonts(), text.WithCollection(gofont.Collection()))
	face := font.Font{Typeface: "Go Mono"}
	gtx := layout.Context{Ops: new(op.Ops), Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1}, Constraints: layout.Exact(image.Pt(2000, 2000))}
	shaper.LayoutString(text.Parameters{Font: face, PxPerEm: fixed.I(14), MaxLines: 1, MaxWidth: 1 << 24}, "MMMMMMMMMM")
	var advance fixed.Int26_6
	var height int
	for g, ok := shaper.NextGlyph(); ok; g, ok = shaper.NextGlyph() {
		advance += g.Advance
		height = (g.Ascent + g.Descent).Ceil()
	}
	cell := (advance / 10).Ceil()
	replica := replicaOf(t, 20, 5, "\x1b[1;31mbold\x1b[0m 漢字 \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\")
	var view View
	replica.Window(&view, replica.ScreenTop(), 5)
	view.Selection, view.HasSelection = replica.Extent(Selection{Anchor: Point{0, 0}, Head: Point{0, 3}}), true
	painter := Painter{Shaper: shaper, Font: face, TextSize: 14}
	dims := painter.Layout(gtx, &view)
	if want := image.Pt(20*cell, 5*height); dims.Size != want {
		t.Fatalf("grid size = %v, want %v", dims.Size, want)
	}
}
