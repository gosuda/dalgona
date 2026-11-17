package tui

import (
	"bytes"
	_ "embed"
	"encoding/base64"
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

const imgRowMark = "\x00IMG"

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
	if product == "dal" {
		if tinted, err := tintPNG(dalPNG, paletteHex(t, roleAccentText)); err == nil {
			return tinted
		}
		return dalPNG
	}
	return dalgonaPNG
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

func logoSeq(product string, proto string, t theme) string {
	if proto == "" {
		return ""
	}
	data := brandPNG(product, t)
	cols, rows := 10, 8
	var seq string
	if proto == "iterm" {
		seq = fmt.Sprintf("\x1b]1337;File=inline=1;preserveAspectRatio=1;width=%d;height=%d:%s\x07",
			cols, rows, base64.StdEncoding.EncodeToString(data))
	} else {
		seq = kittySeq(data, cols, rows)
	}
	return imgRowMark + seq + strings.Repeat("\n", rows)
}

func kittySeq(data []byte, cols, rows int) string {
	enc := base64.StdEncoding.EncodeToString(data)
	var sb strings.Builder
	for len(enc) > 0 {
		n := min(len(enc), 4096)
		chunk := enc[:n]
		enc = enc[n:]
		if sb.Len() == 0 {
			more := 0
			if len(enc) > 0 {
				more = 1
			}
			fmt.Fprintf(&sb, "\x1b_Ga=T,f=100,t=d,q=2,C=1,c=%d,r=%d,m=%d;%s\x1b\\", cols, rows, more, chunk)
		} else {
			more := 0
			if len(enc) > 0 {
				more = 1
			}
			fmt.Fprintf(&sb, "\x1b_Gm=%d;%s\x1b\\", more, chunk)
		}
	}
	return sb.String()
}
