package tui

import (
	"bytes"
	_ "embed"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"strings"
)

//go:embed brand/dal.png
var dalPNG []byte

//go:embed brand/dalgona.png
var dalgonaPNG []byte

func detectImageProto(getenv func(string) string) string {
	if getenv("TMUX") != "" || getenv("STY") != "" {
		return ""
	}
	term := getenv("TERM")
	if getenv("KITTY_WINDOW_ID") != "" || strings.Contains(term, "kitty") ||
		strings.Contains(term, "ghostty") || getenv("WEZTERM_PANE") != "" {
		return "kitty"
	}
	if getenv("TERM_PROGRAM") == "iTerm.app" {
		return "iterm"
	}
	return ""
}

func brandPNG(product string, t theme) []byte {
	switch product {
	case "dal":
		if tinted, err := tintPNG(dalPNG, paletteHex(t, roleAccentText)); err == nil {
			return tinted
		}
		return dalPNG
	case "dalgona":
		return dalgonaPNG
	}
	return nil
}

func tintPNG(src []byte, hex string) ([]byte, error) {
	img, err := png.Decode(bytes.NewReader(src))
	if err != nil {
		return nil, err
	}
	var v uint32
	if _, err := fmt.Sscanf(hex, "#%06x", &v); err != nil {
		return nil, err
	}
	tint := color.NRGBA{R: uint8(v >> 16 & 0xff), G: uint8(v >> 8 & 0xff), B: uint8(v & 0xff)}
	bounds := img.Bounds()
	out := image.NewNRGBA(bounds)
	for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
		for x := bounds.Min.X; x < bounds.Max.X; x++ {
			_, _, _, a := img.At(x, y).RGBA()
			tint.A = uint8(a >> 8 & 0xff)
			out.SetNRGBA(x, y, tint)
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, out); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
