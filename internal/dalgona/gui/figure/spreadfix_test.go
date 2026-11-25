//go:build gui

package figure

import (
	"testing"
)

func partialGradient(spread Spread) Gradient {
	return Gradient{
		Start: Point{X: 0, Y: 0}, End: Point{X: 10, Y: 0},
		Stops:  []Stop{{Offset: 0.2, Color: Color{R: 255, A: 255}}, {Offset: 0.8, Color: Color{B: 255, A: 255}}},
		Spread: spread,
	}
}

func covers(strips []gradientStrip, lo, hi float32) bool {
	at := lo
	for _, s := range strips {
		if s.to <= at || s.from > at {
			continue
		}
		at = s.to
		if at >= hi {
			return true
		}
	}
	return at >= hi
}

func TestPeriodicSpreadsTileTheFullPeriod(t *testing.T) {
	for _, spread := range []Spread{SpreadRepeat, SpreadReflect} {
		strips := spreadStrips(partialGradient(spread), 0, 1)
		if !covers(strips, 0, 1) {
			t.Fatalf("spread %v leaves gaps inside [0, 1]: %+v", spread, strips)
		}
	}
}

func TestSpreadRangeFollowsTheGradientTransform(t *testing.T) {
	lists := &List{Width: 100, Height: 10}
	axis := Gradient{Start: Point{X: 0, Y: 0}, End: Point{X: 10, Y: 0}, Stops: twoStopGradient(SpreadRepeat).Stops}
	_, plain := (&Canvas{List: lists}).spreadRange(axis)
	axis.Space = Affine{A: 2, D: 2}
	_, scaled := (&Canvas{List: lists}).spreadRange(axis)
	if plain != 10 || scaled != 5 {
		t.Fatalf("spread range = %v plain and %v scaled, want 10 and 5", plain, scaled)
	}
}
