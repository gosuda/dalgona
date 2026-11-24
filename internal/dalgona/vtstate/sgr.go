package vtstate

import (
	"image/color"

	"github.com/charmbracelet/x/ansi"
)

var sgrAttributes = map[int]func(*Style){
	1:  func(s *Style) { s.Attributes |= AttrBold },
	2:  func(s *Style) { s.Attributes |= AttrFaint },
	3:  func(s *Style) { s.Attributes |= AttrItalic },
	5:  func(s *Style) { s.Attributes |= AttrBlink },
	6:  func(s *Style) { s.Attributes |= AttrRapidBlink },
	7:  func(s *Style) { s.Attributes |= AttrReverse },
	8:  func(s *Style) { s.Attributes |= AttrConceal },
	9:  func(s *Style) { s.Attributes |= AttrStrikethrough },
	22: func(s *Style) { s.Attributes &^= AttrBold | AttrFaint },
	23: func(s *Style) { s.Attributes &^= AttrItalic },
	24: func(s *Style) { s.Underline = UnderlineNone },
	25: func(s *Style) { s.Attributes &^= AttrBlink | AttrRapidBlink },
	27: func(s *Style) { s.Attributes &^= AttrReverse },
	28: func(s *Style) { s.Attributes &^= AttrConceal },
	29: func(s *Style) { s.Attributes &^= AttrStrikethrough },
	39: func(s *Style) { s.Foreground = Color{} },
	49: func(s *Style) { s.Background = Color{} },
	59: func(s *Style) { s.UnderlineColor = Color{} },
}

func readStyle(params ansi.Params, style *Style) {
	if len(params) == 0 {
		*style = Style{}
		return
	}
	for i := 0; i < len(params); i++ {
		i += readStyleParam(params, i, style)
	}
}

func readStyleParam(params ansi.Params, i int, style *Style) int {
	param, more, _ := params.Param(i, 0)
	if apply, ok := sgrAttributes[param]; ok {
		apply(style)
		return 0
	}
	if target, index, ok := basicColor(style, param); ok {
		*target = IndexedColor(index)
		return 0
	}
	switch param {
	case 0:
		*style = Style{}
	case 4:
		return readUnderline(params, i, more, style)
	case 38:
		return readColor(params[i:], &style.Foreground)
	case 48:
		return readColor(params[i:], &style.Background)
	case 58:
		return readColor(params[i:], &style.UnderlineColor)
	}
	return 0
}

func basicColor(style *Style, param int) (*Color, uint8, bool) {
	switch {
	case param >= 30 && param <= 37:
		return &style.Foreground, uint8(param - 30), true
	case param >= 40 && param <= 47:
		return &style.Background, uint8(param - 40), true
	case param >= 90 && param <= 97:
		return &style.Foreground, uint8(param - 90 + 8), true
	case param >= 100 && param <= 107:
		return &style.Background, uint8(param - 100 + 8), true
	}
	return nil, 0, false
}

func readUnderline(params ansi.Params, i int, more bool, style *Style) int {
	next, _, ok := params.Param(i+1, 0)
	if !more || !ok {
		style.Underline = UnderlineSingle
		return 0
	}
	if next < 0 || next > int(UnderlineDashed) {
		return 0
	}
	style.Underline = Underline(next)
	return 1
}

func readColor(params ansi.Params, target *Color) int {
	var c color.Color
	n := ansi.ReadStyleColor(params, &c)
	if n == 0 {
		return 0
	}
	*target = canonicalColor(c)
	return n - 1
}

func canonicalColor(c color.Color) Color {
	switch v := c.(type) {
	case ansi.IndexedColor:
		return IndexedColor(uint8(v))
	case ansi.BasicColor:
		return IndexedColor(uint8(v))
	case nil:
		return Color{}
	}
	r, g, b, a := c.RGBA()
	if a == 0 {
		return Color{}
	}
	return RGBColor(uint8(r>>8), uint8(g>>8), uint8(b>>8)) //nolint:gosec // RGBA channels shifted by 8 always fit in a byte
}
