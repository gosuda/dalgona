//go:build gui

package figure

import "testing"

func twoStopGradient(spread Spread) Gradient {
	return Gradient{
		Start: Point{X: 0, Y: 0}, End: Point{X: 10, Y: 0},
		Stops:  []Stop{{Offset: 0, Color: Color{R: 255, A: 255}}, {Offset: 1, Color: Color{B: 255, A: 255}}},
		Spread: spread,
	}
}

func TestSpreadStripsPadPaintsEndBands(t *testing.T) {
	strips := spreadStrips(twoStopGradient(SpreadPad), 0, 1)
	if len(strips) != 3 {
		t.Fatalf("pad strips = %d, want pre, middle, and post bands", len(strips))
	}
	if strips[0].from != -gradientReach || strips[0].to != 0 {
		t.Fatalf("pad pre-band = [%v, %v], want [-reach, 0]", strips[0].from, strips[0].to)
	}
	if strips[2].from != 1 || strips[2].to != gradientReach {
		t.Fatalf("pad post-band = [%v, %v], want [1, reach]", strips[2].from, strips[2].to)
	}
}

func TestSpreadStripsRepeatTilesPeriods(t *testing.T) {
	strips := spreadStrips(twoStopGradient(SpreadRepeat), 0, 1)
	if len(strips) != 4 {
		t.Fatalf("repeat strips = %d, want periods covering [-1, 2]", len(strips))
	}
	for k, s := range strips {
		if s.from != float32(k-1) || s.to != float32(k) {
			t.Fatalf("repeat strip %d = [%v, %v], want [%d, %d]", k, s.from, s.to, k-1, k)
		}
	}
}

func checkStrip(t *testing.T, s, want gradientStrip) {
	t.Helper()
	if s.from != want.from || s.to != want.to || s.start.Offset != want.start.Offset || s.end.Offset != want.end.Offset {
		t.Fatalf("strip = %+v, want [%v, %v] from offset %v to %v", s, want.from, want.to, want.start.Offset, want.end.Offset)
	}
}

func TestSpreadStripsReflectMirrorsOddPeriods(t *testing.T) {
	strips := spreadStrips(twoStopGradient(SpreadReflect), 0, 1)
	if len(strips) != 4 {
		t.Fatalf("reflect strips = %d, want periods covering [-1, 2]", len(strips))
	}
	checkStrip(t, strips[0], gradientStrip{from: -1, to: 0, start: Stop{Offset: 1}, end: Stop{}})
	checkStrip(t, strips[1], gradientStrip{from: 0, to: 1, start: Stop{}, end: Stop{Offset: 1}})
}

func TestSpreadStripsSingleStopStaysSolid(t *testing.T) {
	g := Gradient{
		Start: Point{X: 0, Y: 0}, End: Point{X: 10, Y: 0},
		Stops:  []Stop{{Offset: 0.5, Color: Color{G: 255, A: 255}}},
		Spread: SpreadRepeat,
	}
	strips := spreadStrips(g, 0, 1)
	if len(strips) != 2 {
		t.Fatalf("single-stop repeat strips = %d, want solid edge bands", len(strips))
	}
}
