package tui

import (
	"bytes"
	"encoding/base64"
	"image/png"
	"strconv"
	"strings"
	"testing"
)

func markModel(proto string, width, height int) model {
	m := newTestModel(nil, Options{Cwd: "/work"})
	m.conn.product = "dalgona"
	m.screen.imgProto = proto
	m.screen.width = width
	m.screen.height = height
	return m
}

func kittyPayload(t *testing.T, seq string) []byte {
	t.Helper()
	var raw strings.Builder
	for _, seg := range strings.Split(seq, "\x1b\\") {
		head := strings.Index(seg, "\x1b_G")
		if head < 0 {
			continue
		}
		data := seg[head:]
		if i := strings.IndexByte(data, ';'); i >= 0 {
			raw.WriteString(data[i+1:])
		}
	}
	decoded, err := base64.StdEncoding.DecodeString(raw.String())
	if err != nil {
		t.Fatalf("kitty payload is not valid base64: %v", err)
	}
	return decoded
}

func pngAlphaMax(t *testing.T, data []byte) uint8 {
	t.Helper()
	img, err := png.Decode(bytes.NewReader(data))
	if err != nil {
		t.Fatalf("decode png: %v", err)
	}
	var max uint8
	b := img.Bounds()
	for y := b.Min.Y; y < b.Max.Y; y++ {
		for x := b.Min.X; x < b.Max.X; x++ {
			_, _, _, a := img.At(x, y).RGBA()
			if v := uint8((a >> 8) & 0xff); v > max {
				max = v
			}
		}
	}
	return max
}

func TestMarkOverlayKittyAnchorsBottomRight(t *testing.T) {
	t.Parallel()
	m := markModel("kitty", 80, 24)
	got := m.markOverlay()
	if got == "" {
		t.Fatal("markOverlay is empty")
	}
	if !strings.HasPrefix(got, "\x1b[s") || !strings.HasSuffix(got, "\x1b[u") {
		t.Fatalf("markOverlay must save and restore the cursor: %q", got[:16])
	}
	cols := markRows * markCellRatio
	anchor := "\x1b[" + strconv.Itoa(24-markRows+1) + ";" + strconv.Itoa(80-cols+1) + "H"
	if !strings.Contains(got, anchor) {
		t.Fatalf("markOverlay anchor %q missing in %q", anchor, got[:64])
	}
	if !strings.Contains(got, "z=-1") {
		t.Fatal("markOverlay must draw under text")
	}
	if !strings.Contains(got, "r=8") {
		t.Fatal("markOverlay rows missing")
	}
	if strings.Contains(got, "c=") {
		t.Fatal("markOverlay must not fix columns; kitty derives them from the image aspect")
	}
}

func TestMarkOverlayTranslucent(t *testing.T) {
	t.Parallel()
	m := markModel("kitty", 80, 24)
	got := m.markOverlay()
	if got == "" {
		t.Fatal("markOverlay is empty")
	}
	payload := kittyPayload(t, got)
	if len(payload) == 0 {
		t.Fatal("markOverlay kitty payload empty")
	}
	original := pngAlphaMax(t, dalgonaPNG)
	faded := pngAlphaMax(t, payload)
	if faded >= original {
		t.Fatalf("mark alpha %d not below source %d", faded, original)
	}
	want := uint8(float64(original) * markAlpha)
	if faded > want+2 || faded < want-2 {
		t.Fatalf("mark alpha %d, want about %d", faded, want)
	}
}

func TestMarkOverlayFallbacks(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name          string
		proto         string
		width, height int
		product       string
	}{
		{"no proto", "", 80, 24, "dalgona"},
		{"sixel falls back to wordmark", "sixel", 80, 24, "dalgona"},
		{"too short", "kitty", 80, markRows + 1, "dalgona"},
		{"too narrow", "kitty", markRows*markCellRatio + 1, 24, "dalgona"},
		{"unknown product", "kitty", 80, 24, "other"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			m := markModel(tc.proto, tc.width, tc.height)
			m.conn.product = tc.product
			if got := m.markOverlay(); got != "" {
				t.Fatalf("markOverlay = %q, want empty", got)
			}
		})
	}
}

func TestMarkOverlayIterm(t *testing.T) {
	t.Parallel()
	m := markModel("iterm", 80, 24)
	got := m.markOverlay()
	if got == "" {
		t.Fatal("markOverlay is empty")
	}
	if !strings.Contains(got, "\x1b]1337;File=") || !strings.Contains(got, "preserveAspectRatio=1") {
		t.Fatalf("markOverlay iterm framing: %q", got[:80])
	}
	if !strings.Contains(got, ";width=16;height=8:") {
		t.Fatalf("markOverlay iterm cell size: %q", got[:200])
	}
}

func TestMarkKittySequenceChunked(t *testing.T) {
	t.Parallel()
	data := markPixels("dalgona", theme{})
	seq := markKittySequence(data, markRows)
	if !strings.HasPrefix(seq, "\x1b_Ga=T,f=100,i="+strconv.Itoa(markKittyID)) {
		t.Fatalf("kitty head: %q", seq[:64])
	}
	if !strings.Contains(seq, "m=1;") {
		t.Fatal("kitty payload must be chunked")
	}
	if decoded := kittyPayload(t, seq); !bytes.Equal(decoded, data) {
		t.Fatal("kitty payload round trip differs")
	}
}

func TestMarkPixelsCache(t *testing.T) {
	t.Parallel()
	th := theme{name: themeNight, known: true}
	first := markPixels("dalgona", th)
	second := markPixels("dalgona", th)
	if !bytes.Equal(first, second) {
		t.Fatal("markPixels cache unstable")
	}
	if markPixels("other", th) != nil {
		t.Fatal("unknown product must yield no pixels")
	}
}
