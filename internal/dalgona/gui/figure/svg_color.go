package figure

import (
	"math"
	"strconv"
	"strings"
)

type rgba struct{ r, g, b, a float64 }

func (c rgba) color() Color {
	return Color{R: channel(c.r), G: channel(c.g), B: channel(c.b), A: channel(c.a)}
}

func (c rgba) withAlpha(a float64) rgba {
	c.a *= clamp01(a)
	return c
}

func channel(v float64) uint8 { return uint8(math.Round(clamp01(v) * 255)) }

func clamp01(v float64) float64 {
	if math.IsNaN(v) || v < 0 {
		return 0
	}
	return min(v, 1)
}

func (c rgba) luminance() float64 { return 0.2125*c.r + 0.7154*c.g + 0.0721*c.b }

func parseColor(s string) (rgba, bool) {
	s = strings.ToLower(strings.TrimSpace(s))
	if strings.HasPrefix(s, "#") {
		return parseHexColor(s[1:])
	}
	if open := strings.IndexByte(s, '('); open > 0 && strings.HasSuffix(s, ")") {
		return parseColorFunction(strings.TrimSpace(s[:open]), s[open+1:len(s)-1])
	}
	if s == "transparent" {
		return rgba{}, true
	}
	hex, ok := namedColors[s]
	if !ok {
		return rgba{}, false
	}
	return rgba{r: float64(hex>>16) / 255, g: float64(hex>>8&0xff) / 255, b: float64(hex&0xff) / 255, a: 1}, true
}

func parseHexColor(h string) (rgba, bool) {
	v, err := strconv.ParseUint(h, 16, 32)
	if err != nil {
		return rgba{}, false
	}
	switch len(h) {
	case 3:
		return nibbles(v<<4|0xf, 4), true
	case 4:
		return nibbles(v, 4), true
	case 6:
		return bytesColor(v<<8 | 0xff), true
	case 8:
		return bytesColor(v), true
	}
	return rgba{}, false
}

func nibbles(v uint64, n int) rgba {
	part := func(i int) float64 { return float64(v>>(4*(n-1-i))&0xf) / 15 }
	return rgba{r: part(0), g: part(1), b: part(2), a: part(3)}
}

func bytesColor(v uint64) rgba {
	return rgba{r: float64(v>>24&0xff) / 255, g: float64(v>>16&0xff) / 255, b: float64(v>>8&0xff) / 255, a: float64(v&0xff) / 255}
}

func colorArguments(body string) []string {
	body = strings.ReplaceAll(body, "/", " / ")
	if strings.Contains(body, ",") {
		parts := strings.Split(body, ",")
		for i := range parts {
			parts[i] = strings.TrimSpace(parts[i])
		}
		return parts
	}
	fields := strings.Fields(body)
	if len(fields) == 5 && fields[3] == "/" {
		return []string{fields[0], fields[1], fields[2], fields[4]}
	}
	return fields
}

func parseColorFunction(name, body string) (rgba, bool) {
	args := colorArguments(body)
	if len(args) != 3 && len(args) != 4 {
		return rgba{}, false
	}
	alpha := 1.0
	if len(args) == 4 {
		a, ok := fraction(args[3], 1)
		if !ok {
			return rgba{}, false
		}
		alpha = a
	}
	switch name {
	case "rgb", "rgba":
		return rgbArguments(args[:3], alpha)
	case "hsl", "hsla":
		return hslArguments(args[:3], alpha)
	}
	return rgba{}, false
}

func fraction(s string, scale float64) (float64, bool) {
	if p, ok := strings.CutSuffix(s, "%"); ok {
		v, err := strconv.ParseFloat(p, 64)
		return clamp01(v / 100), err == nil
	}
	v, err := strconv.ParseFloat(s, 64)
	return clamp01(v / scale), err == nil
}

func rgbArguments(args []string, alpha float64) (rgba, bool) {
	var v [3]float64
	for i, a := range args {
		f, ok := fraction(a, 255)
		if !ok {
			return rgba{}, false
		}
		v[i] = f
	}
	return rgba{r: v[0], g: v[1], b: v[2], a: alpha}, true
}

func hslArguments(args []string, alpha float64) (rgba, bool) {
	h, err := strconv.ParseFloat(strings.TrimSuffix(args[0], "deg"), 64)
	s, okS := fraction(args[1], 100)
	l, okL := fraction(args[2], 100)
	if err != nil || !okS || !okL || math.IsInf(h, 0) {
		return rgba{}, false
	}
	h = math.Mod(math.Mod(h, 360)+360, 360) / 360
	q := l + s - l*s
	if l < 0.5 {
		q = l * (1 + s)
	}
	p := 2*l - q
	return rgba{r: hue(p, q, h+1.0/3), g: hue(p, q, h), b: hue(p, q, h-1.0/3), a: alpha}, true
}

func hue(p, q, t float64) float64 {
	t = math.Mod(t+1, 1)
	switch {
	case t < 1.0/6:
		return p + (q-p)*6*t
	case t < 0.5:
		return q
	case t < 2.0/3:
		return p + (q-p)*(2.0/3-t)*6
	}
	return p
}

var namedColors = map[string]uint32{
	"aliceblue": 0xf0f8ff, "antiquewhite": 0xfaebd7, "aqua": 0x00ffff, "aquamarine": 0x7fffd4, "azure": 0xf0ffff,
	"beige": 0xf5f5dc, "bisque": 0xffe4c4, "black": 0x000000, "blanchedalmond": 0xffebcd, "blue": 0x0000ff,
	"blueviolet": 0x8a2be2, "brown": 0xa52a2a, "burlywood": 0xdeb887, "cadetblue": 0x5f9ea0, "chartreuse": 0x7fff00,
	"chocolate": 0xd2691e, "coral": 0xff7f50, "cornflowerblue": 0x6495ed, "cornsilk": 0xfff8dc, "crimson": 0xdc143c,
	"cyan": 0x00ffff, "darkblue": 0x00008b, "darkcyan": 0x008b8b, "darkgoldenrod": 0xb8860b, "darkgray": 0xa9a9a9,
	"darkgreen": 0x006400, "darkgrey": 0xa9a9a9, "darkkhaki": 0xbdb76b, "darkmagenta": 0x8b008b, "darkolivegreen": 0x556b2f,
	"darkorange": 0xff8c00, "darkorchid": 0x9932cc, "darkred": 0x8b0000, "darksalmon": 0xe9967a, "darkseagreen": 0x8fbc8f,
	"darkslateblue": 0x483d8b, "darkslategray": 0x2f4f4f, "darkslategrey": 0x2f4f4f, "darkturquoise": 0x00ced1, "darkviolet": 0x9400d3,
	"deeppink": 0xff1493, "deepskyblue": 0x00bfff, "dimgray": 0x696969, "dimgrey": 0x696969, "dodgerblue": 0x1e90ff,
	"firebrick": 0xb22222, "floralwhite": 0xfffaf0, "forestgreen": 0x228b22, "fuchsia": 0xff00ff, "gainsboro": 0xdcdcdc,
	"ghostwhite": 0xf8f8ff, "gold": 0xffd700, "goldenrod": 0xdaa520, "gray": 0x808080, "green": 0x008000,
	"greenyellow": 0xadff2f, "grey": 0x808080, "honeydew": 0xf0fff0, "hotpink": 0xff69b4, "indianred": 0xcd5c5c,
	"indigo": 0x4b0082, "ivory": 0xfffff0, "khaki": 0xf0e68c, "lavender": 0xe6e6fa, "lavenderblush": 0xfff0f5,
	"lawngreen": 0x7cfc00, "lemonchiffon": 0xfffacd, "lightblue": 0xadd8e6, "lightcoral": 0xf08080, "lightcyan": 0xe0ffff,
	"lightgoldenrodyellow": 0xfafad2, "lightgray": 0xd3d3d3, "lightgreen": 0x90ee90, "lightgrey": 0xd3d3d3, "lightpink": 0xffb6c1,
	"lightsalmon": 0xffa07a, "lightseagreen": 0x20b2aa, "lightskyblue": 0x87cefa, "lightslategray": 0x778899, "lightslategrey": 0x778899,
	"lightsteelblue": 0xb0c4de, "lightyellow": 0xffffe0, "lime": 0x00ff00, "limegreen": 0x32cd32, "linen": 0xfaf0e6,
	"magenta": 0xff00ff, "maroon": 0x800000, "mediumaquamarine": 0x66cdaa, "mediumblue": 0x0000cd, "mediumorchid": 0xba55d3,
	"mediumpurple": 0x9370db, "mediumseagreen": 0x3cb371, "mediumslateblue": 0x7b68ee, "mediumspringgreen": 0x00fa9a, "mediumturquoise": 0x48d1cc,
	"mediumvioletred": 0xc71585, "midnightblue": 0x191970, "mintcream": 0xf5fffa, "mistyrose": 0xffe4e1, "moccasin": 0xffe4b5,
	"navajowhite": 0xffdead, "navy": 0x000080, "oldlace": 0xfdf5e6, "olive": 0x808000, "olivedrab": 0x6b8e23,
	"orange": 0xffa500, "orangered": 0xff4500, "orchid": 0xda70d6, "palegoldenrod": 0xeee8aa, "palegreen": 0x98fb98,
	"paleturquoise": 0xafeeee, "palevioletred": 0xdb7093, "papayawhip": 0xffefd5, "peachpuff": 0xffdab9, "peru": 0xcd853f,
	"pink": 0xffc0cb, "plum": 0xdda0dd, "powderblue": 0xb0e0e6, "purple": 0x800080, "rebeccapurple": 0x663399,
	"red": 0xff0000, "rosybrown": 0xbc8f8f, "royalblue": 0x4169e1, "saddlebrown": 0x8b4513, "salmon": 0xfa8072,
	"sandybrown": 0xf4a460, "seagreen": 0x2e8b57, "seashell": 0xfff5ee, "sienna": 0xa0522d, "silver": 0xc0c0c0,
	"skyblue": 0x87ceeb, "slateblue": 0x6a5acd, "slategray": 0x708090, "slategrey": 0x708090, "snow": 0xfffafa,
	"springgreen": 0x00ff7f, "steelblue": 0x4682b4, "tan": 0xd2b48c, "teal": 0x008080, "thistle": 0xd8bfd8,
	"tomato": 0xff6347, "turquoise": 0x40e0d0, "violet": 0xee82ee, "wheat": 0xf5deb3, "white": 0xffffff,
	"whitesmoke": 0xf5f5f5, "yellow": 0xffff00, "yellowgreen": 0x9acd32,
}
