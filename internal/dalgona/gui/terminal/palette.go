package terminal

import (
	"image/color"
	"math"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

type Palette struct {
	Foreground color.NRGBA
	Background color.NRGBA
	Cursor     color.NRGBA
	Selection  color.NRGBA
	Boundary   color.NRGBA
	ANSI       [16]color.NRGBA
}

var cubeLevels = [6]uint8{0, 95, 135, 175, 215, 255}

func (p *Palette) indexed(i uint8) color.NRGBA {
	switch {
	case i < 16:
		return p.ANSI[i]
	case i < 232:
		n := i - 16
		return color.NRGBA{R: cubeLevels[n/36], G: cubeLevels[n/6%6], B: cubeLevels[n%6], A: 0xff}
	}
	gray := 8 + 10*(i-232)
	return color.NRGBA{R: gray, G: gray, B: gray, A: 0xff}
}

func (p *Palette) resolve(c vtstate.Color, fallback color.NRGBA) color.NRGBA {
	switch c.Kind {
	case vtstate.ColorIndexed:
		return p.indexed(c.Index)
	case vtstate.ColorRGB:
		return color.NRGBA{R: c.Red, G: c.Green, B: c.Blue, A: 0xff}
	case vtstate.ColorDefault:
		return fallback
	}
	return fallback
}

func (p *Palette) colors(s vtstate.Style) (foreground, background color.NRGBA) {
	foreground = p.resolve(s.Foreground, p.Foreground)
	background = p.resolve(s.Background, p.Background)
	if s.Attributes&vtstate.AttrReverse != 0 {
		foreground, background = background, foreground
	}
	if s.Attributes&vtstate.AttrFaint != 0 {
		foreground = blend(foreground, background)
	}
	if s.Attributes&vtstate.AttrConceal != 0 {
		foreground = background
	}
	return foreground, background
}

func blend(a, b color.NRGBA) color.NRGBA {
	return color.NRGBA{
		R: uint8(min((uint16(a.R)+uint16(b.R))/2, math.MaxUint8)),
		G: uint8(min((uint16(a.G)+uint16(b.G))/2, math.MaxUint8)),
		B: uint8(min((uint16(a.B)+uint16(b.B))/2, math.MaxUint8)),
		A: a.A,
	}
}
