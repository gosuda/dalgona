package tui

import (
	"bytes"
	"encoding/base64"
	"image"
	"image/color"
	"image/png"
	"strconv"
	"strings"
	"sync"
)

const (
	markRows       = 8
	markCellRatio  = 2
	markAlpha      = 0.45
	markKittyID    = 2141
	markKittyChunk = 4096
)

var markCache sync.Map

func (m model) markOverlay() string {
	proto := m.screen.imgProto
	if proto != "kitty" && proto != "iterm" {
		return ""
	}
	data := markPixels(m.conn.product, m.screen.dockTheme())
	if len(data) == 0 {
		return ""
	}
	anchor, ok := markAnchor(data, m.screen.width, m.screen.height)
	if !ok {
		return ""
	}
	var seq string
	if proto == "kitty" {
		seq = markKittySequence(data, markRows)
	} else {
		seq = markItermSequence(m.conn.product+".png", data, anchor.cols, markRows)
	}
	return "\x1b[s\x1b[" + strconv.Itoa(anchor.row) + ";" + strconv.Itoa(anchor.col) + "H" + seq + "\x1b[u"
}

type markPlacement struct {
	row, col, cols int
}

func markAnchor(data []byte, width, height int) (markPlacement, bool) {
	srcW, srcH, ok := markPNGSize(data)
	if !ok || srcW <= 0 || srcH <= 0 || height < markRows+2 {
		return markPlacement{}, false
	}
	cols := markRows * markCellRatio * srcW / srcH
	if cols < 1 || cols+2 > width {
		return markPlacement{}, false
	}
	return markPlacement{row: height - markRows + 1, col: width - cols + 1, cols: cols}, true
}

func markPixels(product string, t theme) []byte {
	key := product + "|" + paletteHex(t, roleAccentText)
	if cached, ok := markCache.Load(key); ok {
		return cached.([]byte)
	}
	data := fadePNG(brandPNG(product, t), markAlpha)
	if cached, loaded := markCache.LoadOrStore(key, data); loaded {
		return cached.([]byte)
	}
	return data
}

func fadePNG(src []byte, alpha float64) []byte {
	img, err := png.Decode(bytes.NewReader(src))
	if err != nil {
		return src
	}
	bounds := img.Bounds()
	out := image.NewNRGBA(bounds)
	for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
		for x := bounds.Min.X; x < bounds.Max.X; x++ {
			c := color.NRGBAModel.Convert(img.At(x, y)).(color.NRGBA)
			c.A = uint8(float64(c.A) * alpha)
			out.SetNRGBA(x, y, c)
		}
	}
	var buf bytes.Buffer
	if err := png.Encode(&buf, out); err != nil {
		return src
	}
	return buf.Bytes()
}

func markKittySequence(payload []byte, rows int) string {
	width, height, ok := markPNGSize(payload)
	if !ok || width <= 0 || height <= 0 || rows <= 0 {
		return ""
	}
	encoded := base64.StdEncoding.EncodeToString(payload)
	head := "\x1b_Ga=T,f=100,i=" + strconv.Itoa(markKittyID) + ",q=2,C=1,z=-1,s=" + strconv.Itoa(width) + ",v=" + strconv.Itoa(height) + ",r=" + strconv.Itoa(rows)
	if len(encoded) <= markKittyChunk {
		return head + ";" + encoded + "\x1b\\"
	}
	var out strings.Builder
	for start := 0; start < len(encoded); {
		end := min(start+markKittyChunk, len(encoded))
		more := "m=0;"
		if end < len(encoded) {
			more = "m=1;"
		}
		if start == 0 {
			out.WriteString(head + ",")
		} else {
			out.WriteString("\x1b_G")
		}
		out.WriteString(more + encoded[start:end] + "\x1b\\")
		start = end
	}
	return out.String()
}

func markItermSequence(name string, payload []byte, cols, rows int) string {
	if name == "" || len(payload) == 0 || rows <= 0 || cols <= 0 {
		return ""
	}
	return "\x1b]1337;File=name=" + base64.StdEncoding.EncodeToString([]byte(name)) +
		";size=" + strconv.Itoa(len(payload)) +
		";inline=1;preserveAspectRatio=1;width=" + strconv.Itoa(cols) +
		";height=" + strconv.Itoa(rows) +
		":" + base64.StdEncoding.EncodeToString(payload) + "\a"
}

func markPNGSize(payload []byte) (int, int, bool) {
	config, err := png.DecodeConfig(bytes.NewReader(payload))
	if err != nil {
		return 0, 0, false
	}
	return config.Width, config.Height, true
}
