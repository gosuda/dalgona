package terminal

import (
	"image/color"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

func TestIndexedColorsFollowTheXterm256Palette(t *testing.T) {
	p := Palette{ANSI: [16]color.NRGBA{1: {R: 0xcd, A: 0xff}}}
	cases := map[uint8]color.NRGBA{
		1:   {R: 0xcd, A: 0xff},
		16:  {A: 0xff},
		21:  {B: 0xff, A: 0xff},
		46:  {G: 0xff, A: 0xff},
		202: {R: 0xff, G: 0x5f, A: 0xff},
		231: {R: 0xff, G: 0xff, B: 0xff, A: 0xff},
		232: {R: 0x08, G: 0x08, B: 0x08, A: 0xff},
		244: {R: 0x80, G: 0x80, B: 0x80, A: 0xff},
		255: {R: 0xee, G: 0xee, B: 0xee, A: 0xff},
	}
	for index, want := range cases {
		if got := p.resolve(vtstate.IndexedColor(index), color.NRGBA{}); got != want {
			t.Fatalf("color %d = %v, want %v", index, got, want)
		}
	}
}

func TestStyleAttributesChangeDrawnColors(t *testing.T) {
	fg := color.NRGBA{R: 200, G: 200, B: 200, A: 0xff}
	bg := color.NRGBA{R: 10, G: 20, B: 30, A: 0xff}
	p := Palette{Foreground: fg, Background: bg}
	red := vtstate.RGBColor(250, 0, 0)
	cases := []struct {
		name   string
		style  vtstate.Style
		fg, bg color.NRGBA
	}{
		{"defaults", vtstate.Style{}, fg, bg},
		{"truecolor", vtstate.Style{Foreground: red}, color.NRGBA{R: 250, A: 0xff}, bg},
		{"reverse", vtstate.Style{Attributes: vtstate.AttrReverse}, bg, fg},
		{"faint", vtstate.Style{Attributes: vtstate.AttrFaint}, color.NRGBA{R: 105, G: 110, B: 115, A: 0xff}, bg},
		{"conceal", vtstate.Style{Attributes: vtstate.AttrConceal}, bg, bg},
	}
	for _, c := range cases {
		gotFg, gotBg := p.colors(c.style)
		if gotFg != c.fg || gotBg != c.bg {
			t.Fatalf("%s: colors = %v %v, want %v %v", c.name, gotFg, gotBg, c.fg, c.bg)
		}
	}
}
