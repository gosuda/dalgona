package math

import (
	"errors"
	stdmath "math"
	"testing"
)

func outlineBounds(segments []Segment) [4]float32 {
	bounds := [4]float32{float32(stdmath.Inf(1)), float32(stdmath.Inf(1)), float32(stdmath.Inf(-1)), float32(stdmath.Inf(-1))}
	for _, segment := range segments {
		for _, p := range segment.points() {
			bounds = [4]float32{min(bounds[0], p.X), min(bounds[1], p.Y), max(bounds[2], p.X), max(bounds[3], p.Y)}
		}
	}
	return bounds
}

func inkBox(glyph Glyph, ascent float32) [4]float32 {
	box := inkByID(glyph)
	scale := glyph.Size / 1000
	return [4]float32{
		glyph.X + box[0]*scale,
		ascent + glyph.Y - box[3]*scale,
		glyph.X + box[2]*scale,
		ascent + glyph.Y - box[1]*scale,
	}
}

func layoutInkBoxes(out *Layout) [][4]float32 {
	var boxes [][4]float32
	for _, glyph := range out.Glyphs {
		boxes = append(boxes, inkBox(glyph, out.Ascent))
	}
	for _, rule := range out.Rules {
		boxes = append(boxes, [4]float32{rule.X, out.Ascent + rule.Y, rule.X + rule.Width, out.Ascent + rule.Y + rule.Height})
	}
	for _, run := range out.Texts {
		width := float32(len([]rune(run.Text))) * run.Size / 2
		boxes = append(boxes, [4]float32{run.X, out.Ascent + run.Y - 0.7*run.Size, run.X + width, out.Ascent + run.Y + 0.2*run.Size})
	}
	return boxes
}

func TestPathsPlaceGlyphInkAtLayoutPositions(t *testing.T) {
	out := render(t, `\frac{a}{y}+\sqrt{x}\text{ab}`, false)
	var paths [][4]float32
	err := out.Paths(func(path []Segment) error {
		paths = append(paths, outlineBounds(path))
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	want := layoutInkBoxes(out)
	if len(paths) != len(want) {
		t.Fatalf("Paths visited %d paths, want %d", len(paths), len(want))
	}
	for i, box := range want {
		nearBox(t, "path bound", paths[i], box)
		if paths[i][1] < -1e-2 || paths[i][3] > out.Ascent+out.Descent+1e-2 {
			t.Errorf("path %d ink %v leaves the formula box of height %v", i, paths[i], out.Ascent+out.Descent)
		}
	}
}

func TestPathsStopAtTheFirstVisitError(t *testing.T) {
	out := render(t, `a+b`, false)
	visits := 0
	stop := errLayoutBudget
	if err := out.Paths(func([]Segment) error { visits++; return stop }); !errors.Is(err, stop) || visits != 1 {
		t.Fatalf("Paths returned %v after %d visits, want the visit error after 1", err, visits)
	}
}
