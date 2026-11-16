package tui

import (
	"bytes"
	_ "embed"
	"encoding/base64"
	"image/png"
	"os"
	"strconv"
	"strings"
)

//go:embed brand/dalgona-8.png
var markDalgonaSmallPNG []byte

//go:embed brand/dalgona-16.png
var markDalgonaLargePNG []byte

//go:embed brand/dal-8.png
var markDalSmallPNG []byte

//go:embed brand/dal-16.png
var markDalLargePNG []byte

//go:embed brand/dalgona-8.sixel
var markDalgonaSmallSixel []byte

//go:embed brand/dalgona-16.sixel
var markDalgonaLargeSixel []byte

//go:embed brand/dal-8.sixel
var markDalSmallSixel []byte

//go:embed brand/dal-16.sixel
var markDalLargeSixel []byte

type markProtocol uint8

const (
	markNone markProtocol = iota
	markKitty
	markIterm
	markSixel
)

const (
	markSmallRows  = 8
	markLargeRows  = 16
	markKittyChunk = 4096
)

type markArt struct {
	name  string
	png   []byte
	sixel []byte
	rows  int
}

func MarkImage(product string, height int) string {
	return markImageWith(os.Getenv, product, height)
}

func markImageWith(getenv func(string) string, product string, height int) string {
	protocol := detectMarkProtocol(getenv)
	if protocol == markNone {
		return ""
	}
	art, ok := markArtFor(product, height)
	if !ok {
		return ""
	}
	switch protocol {
	case markKitty:
		return markKittySequence(art.png, art.rows)
	case markIterm:
		return markItermSequence(art.name, art.png, art.rows)
	case markSixel:
		return string(art.sixel)
	case markNone:
		return ""
	}
	return ""
}

func detectMarkProtocol(getenv func(string) string) markProtocol {
	if getenv("TMUX") != "" || getenv("STY") != "" {
		return markNone
	}
	term := getenv("TERM")
	if term == "" || term == "dumb" {
		return markNone
	}
	if protocol, ok := markTerminalProgram(getenv, term); ok {
		return protocol
	}
	if strings.Contains(strings.ToLower(term), "sixel") {
		return markSixel
	}
	return markNone
}

func markTerminalProgram(getenv func(string) string, term string) (markProtocol, bool) {
	program := getenv("TERM_PROGRAM")
	if term == "xterm-kitty" || getenv("KITTY_WINDOW_ID") != "" || program == "kitty" || program == "ghostty" {
		return markKitty, true
	}
	switch program {
	case "iTerm.app", "iTerm2", "iTerm", "WezTerm", "vscode":
		return markIterm, true
	}
	if getenv("ITERM_SESSION_ID") != "" {
		return markIterm, true
	}
	return markNone, false
}

func markArtFor(product string, height int) (markArt, bool) {
	rows := markLargeRows
	if height <= (markSmallRows+markLargeRows)/2 {
		rows = markSmallRows
	}
	small := rows == markSmallRows
	switch strings.ToLower(product) {
	case "dalgona":
		if small {
			return markArt{name: "dalgona-8.png", png: markDalgonaSmallPNG, sixel: markDalgonaSmallSixel, rows: rows}, true
		}
		return markArt{name: "dalgona-16.png", png: markDalgonaLargePNG, sixel: markDalgonaLargeSixel, rows: rows}, true
	case "dal":
		if small {
			return markArt{name: "dal-8.png", png: markDalSmallPNG, sixel: markDalSmallSixel, rows: rows}, true
		}
		return markArt{name: "dal-16.png", png: markDalLargePNG, sixel: markDalLargeSixel, rows: rows}, true
	default:
		return markArt{}, false
	}
}

func markKittySequence(payload []byte, rows int) string {
	width, height, ok := markPNGSize(payload)
	if !ok || width <= 0 || height <= 0 || rows <= 0 {
		return ""
	}
	encoded := base64.StdEncoding.EncodeToString(payload)
	head := "\x1b_Ga=T,f=100,s=" + strconv.Itoa(width) + ",v=" + strconv.Itoa(height) + ",r=" + strconv.Itoa(rows)
	if len(encoded) <= markKittyChunk {
		return head + ";" + encoded + "\x1b\\"
	}
	var out strings.Builder
	for start := 0; start < len(encoded); {
		end := min(start+markKittyChunk, len(encoded))
		more := ",m=0;"
		if end < len(encoded) {
			more = ",m=1;"
		}
		if start == 0 {
			out.WriteString(head)
		} else {
			out.WriteString("\x1b_G")
		}
		out.WriteString(more + encoded[start:end] + "\x1b\\")
		start = end
	}
	return out.String()
}

func markItermSequence(name string, payload []byte, rows int) string {
	if name == "" || len(payload) == 0 || rows <= 0 {
		return ""
	}
	return "\x1b]1337;File=name=" + base64.StdEncoding.EncodeToString([]byte(name)) +
		";size=" + strconv.Itoa(len(payload)) +
		";inline=1;height=" + strconv.Itoa(rows) +
		":" + base64.StdEncoding.EncodeToString(payload) + "\a"
}

func markPNGSize(payload []byte) (int, int, bool) {
	config, err := png.DecodeConfig(bytes.NewReader(payload))
	if err != nil {
		return 0, 0, false
	}
	return config.Width, config.Height, true
}
