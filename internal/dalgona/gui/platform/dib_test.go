package platform

import (
	"bytes"
	"encoding/binary"
	"errors"
	"image"
	"image/color"
	"image/png"
	"math"
	"testing"
)

const (
	dibTestCompressionRLE8  = 1
	dibTestCompressionPNG   = 5
	dibTestColorSpaceSRGB   = 0x73524742
	dibTestIntentImages     = 4
	dibTestColorsUsedOffset = 32
)

type dibFixture struct {
	headerSize  uint32
	width       int32
	height      int32
	planes      uint16
	bitCount    uint16
	compression uint32
	colorsUsed  uint32
	masks       [4]uint32
	pixels      []byte
}

func (f dibFixture) encode() []byte {
	var out bytes.Buffer
	write := func(values ...any) {
		for _, value := range values {
			if err := binary.Write(&out, binary.LittleEndian, value); err != nil {
				panic(err)
			}
		}
	}
	if len(f.pixels) > transferLimit {
		panic("DIB fixture exceeds the transfer limit")
	}
	write(f.headerSize, f.width, f.height, f.planes, f.bitCount, f.compression, uint32(len(f.pixels)), int32(2835), int32(2835), f.colorsUsed, uint32(0)) //nolint:gosec // the fixture length is bounded by the 32 MiB transfer limit
	switch f.headerSize {
	case dibInfoHeaderSize:
		if f.compression == dibCompressionBitfields {
			write(f.masks[:3])
		}
	case dibV4HeaderSize:
		write(f.masks, uint32(dibTestColorSpaceSRGB), [9]int32{}, [3]uint32{})
	case dibV5HeaderSize:
		write(f.masks, uint32(dibTestColorSpaceSRGB), [9]int32{}, [3]uint32{}, uint32(dibTestIntentImages), uint32(0), uint32(0), uint32(0))
	}
	for i := range f.colorsUsed {
		write([4]byte{0xA0, 0xB0, 0xC0, byte(i)})
	}
	out.Write(f.pixels)
	return out.Bytes()
}

func dibBGRRow(padding int, colors ...[3]byte) []byte {
	var row []byte
	for _, c := range colors {
		row = append(row, c[2], c[1], c[0])
	}
	return append(row, bytes.Repeat([]byte{0xEE}, padding)...)
}

func dibDWORDRow(pixels ...uint32) []byte {
	row := make([]byte, 0, 4*len(pixels))
	for _, p := range pixels {
		row = binary.LittleEndian.AppendUint32(row, p)
	}
	return row
}

func dibRows(rows ...[]byte) []byte {
	return bytes.Join(rows, nil)
}

var (
	dibRed   = [3]byte{0xFF, 0x00, 0x00}
	dibGreen = [3]byte{0x00, 0xFF, 0x00}
	dibBlue  = [3]byte{0x00, 0x00, 0xFF}
	dibWhite = [3]byte{0xFF, 0xFF, 0xFF}
	dibBlack = [3]byte{0x00, 0x00, 0x00}
	dibMixed = [3]byte{0x12, 0x34, 0x56}
)

func dibOpaque(c [3]byte) color.NRGBA {
	return color.NRGBA{R: c[0], G: c[1], B: c[2], A: 0xFF}
}

func dibStandardMasks(alpha uint32) [4]uint32 {
	return [4]uint32{0x00FF0000, 0x0000FF00, 0x000000FF, alpha}
}

func TestDIBDecodesDocumentedLayouts(t *testing.T) {
	cases := []struct {
		name    string
		fixture dibFixture
		want    [][]color.NRGBA
	}{
		{
			name: "24-bit bottom-up rows keep the last stored row on top and skip row padding",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 3, height: 2, planes: 1, bitCount: 24,
				pixels: dibRows(dibBGRRow(3, dibRed, dibGreen, dibBlue), dibBGRRow(3, dibWhite, dibBlack, dibMixed)),
			},
			want: [][]color.NRGBA{
				{dibOpaque(dibWhite), dibOpaque(dibBlack), dibOpaque(dibMixed)},
				{dibOpaque(dibRed), dibOpaque(dibGreen), dibOpaque(dibBlue)},
			},
		},
		{
			name: "24-bit top-down rows keep the first stored row on top",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 3, height: -2, planes: 1, bitCount: 24,
				pixels: dibRows(dibBGRRow(3, dibRed, dibGreen, dibBlue), dibBGRRow(3, dibWhite, dibBlack, dibMixed)),
			},
			want: [][]color.NRGBA{
				{dibOpaque(dibRed), dibOpaque(dibGreen), dibOpaque(dibBlue)},
				{dibOpaque(dibWhite), dibOpaque(dibBlack), dibOpaque(dibMixed)},
			},
		},
		{
			name: "24-bit one-pixel rows pad to a DWORD",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 1, height: -3, planes: 1, bitCount: 24,
				pixels: dibRows(dibBGRRow(1, dibRed), dibBGRRow(1, dibGreen), dibBGRRow(1, dibBlue)),
			},
			want: [][]color.NRGBA{{dibOpaque(dibRed)}, {dibOpaque(dibGreen)}, {dibOpaque(dibBlue)}},
		},
		{
			name: "24-bit color table entries before the pixels are skipped",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 1, height: 1, planes: 1, bitCount: 24, colorsUsed: 3,
				pixels: dibBGRRow(1, dibMixed),
			},
			want: [][]color.NRGBA{{dibOpaque(dibMixed)}},
		},
		{
			name: "32-bit BI_RGB with every high byte zero is opaque",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 2, height: 1, planes: 1, bitCount: 32,
				pixels: dibDWORDRow(0x00123456, 0x00FF0000),
			},
			want: [][]color.NRGBA{{dibOpaque(dibMixed), dibOpaque(dibRed)}},
		},
		{
			name: "32-bit BI_RGB with a nonzero high byte keeps it as straight alpha",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 2, height: 1, planes: 1, bitCount: 32,
				pixels: dibDWORDRow(0x80FF0000, 0x000000FF),
			},
			want: [][]color.NRGBA{{{R: 0xFF, A: 0x80}, {B: 0xFF}}},
		},
		{
			name: "BITMAPINFOHEADER bit fields follow the header and precede the color table",
			fixture: dibFixture{
				headerSize: dibInfoHeaderSize, width: 1, height: 1, planes: 1, bitCount: 32,
				compression: dibCompressionBitfields, colorsUsed: 2,
				masks:  [4]uint32{0x000000FF, 0x0000FF00, 0x00FF0000},
				pixels: dibDWORDRow(0xFF563412),
			},
			want: [][]color.NRGBA{{dibOpaque(dibMixed)}},
		},
		{
			name: "BITMAPV4HEADER bit fields take alpha from the alpha mask",
			fixture: dibFixture{
				headerSize: dibV4HeaderSize, width: 2, height: 1, planes: 1, bitCount: 32,
				compression: dibCompressionBitfields, masks: dibStandardMasks(0xFF000000),
				pixels: dibDWORDRow(0x40102030, 0xFF123456),
			},
			want: [][]color.NRGBA{{{R: 0x10, G: 0x20, B: 0x30, A: 0x40}, dibOpaque(dibMixed)}},
		},
		{
			name: "BITMAPV5HEADER BI_RGB takes alpha from the alpha mask",
			fixture: dibFixture{
				headerSize: dibV5HeaderSize, width: 1, height: 1, planes: 1, bitCount: 32,
				masks: dibStandardMasks(0xFF000000), pixels: dibDWORDRow(0x7F0000FF),
			},
			want: [][]color.NRGBA{{{B: 0xFF, A: 0x7F}}},
		},
		{
			name: "BITMAPV5HEADER without an alpha mask is opaque",
			fixture: dibFixture{
				headerSize: dibV5HeaderSize, width: 1, height: 1, planes: 1, bitCount: 32,
				masks: dibStandardMasks(0), pixels: dibDWORDRow(0x7F0000FF),
			},
			want: [][]color.NRGBA{{dibOpaque(dibBlue)}},
		},
		{
			name: "BITMAPV5HEADER whose alpha samples are all zero stays transparent",
			fixture: dibFixture{
				headerSize: dibV5HeaderSize, width: 2, height: 1, planes: 1, bitCount: 32,
				compression: dibCompressionBitfields, masks: dibStandardMasks(0xFF000000),
				pixels: dibDWORDRow(0x00FF0000, 0x00123456),
			},
			want: [][]color.NRGBA{{{R: 0xFF}, {R: 0x12, G: 0x34, B: 0x56}}},
		},
		{
			name: "BITMAPV5HEADER 10-bit and 2-bit masks scale to the nearest 8-bit level",
			fixture: dibFixture{
				headerSize: dibV5HeaderSize, width: 1, height: 1, planes: 1, bitCount: 32,
				compression: dibCompressionBitfields,
				masks:       [4]uint32{0x3FF00000, 0x000FFC00, 0x000003FF, 0xC0000000},
				pixels:      dibDWORDRow(2<<30 | 0x3FF<<20 | 0x200<<10 | 0x001),
			},
			want: [][]color.NRGBA{{{R: 0xFF, G: 0x80, B: 0x00, A: 0xAA}}},
		},
		{
			name: "BITMAPV5HEADER 24-bit pixels hold no bits of a high alpha mask",
			fixture: dibFixture{
				headerSize: dibV5HeaderSize, width: 1, height: 1, planes: 1, bitCount: 24,
				masks: dibStandardMasks(0xFF000000), pixels: dibBGRRow(1, dibMixed),
			},
			want: [][]color.NRGBA{{dibOpaque(dibMixed)}},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			encoded, err := pngFromDIB(tc.fixture.encode())
			if err != nil {
				t.Fatalf("pngFromDIB: %v", err)
			}
			img, err := png.Decode(bytes.NewReader(encoded))
			if err != nil {
				t.Fatalf("the result is not a PNG: %v", err)
			}
			if got := img.Bounds(); got != image.Rect(0, 0, len(tc.want[0]), len(tc.want)) {
				t.Fatalf("bounds %v, want %d×%d", got, len(tc.want[0]), len(tc.want))
			}
			checkDIBPixels(t, img, tc.want)
		})
	}
}

func checkDIBPixels(t *testing.T, img image.Image, pixels [][]color.NRGBA) {
	t.Helper()
	for y, row := range pixels {
		for x, want := range row {
			if got := color.NRGBAModel.Convert(img.At(x, y)); got != want {
				t.Errorf("pixel (%d, %d) = %#v, want %#v", x, y, got, want)
			}
		}
	}
}

func TestDIBRefusesMalformedBitmaps(t *testing.T) {
	valid := dibFixture{
		headerSize: dibInfoHeaderSize, width: 3, height: 2, planes: 1, bitCount: 24,
		pixels: dibRows(dibBGRRow(3, dibRed, dibGreen, dibBlue), dibBGRRow(3, dibWhite, dibBlack, dibMixed)),
	}
	with := func(change func(*dibFixture)) []byte {
		f := valid
		change(&f)
		return f.encode()
	}
	full := valid.encode()
	v5 := with(func(f *dibFixture) { f.headerSize = dibV5HeaderSize })
	hugeTable := valid.encode()
	binary.LittleEndian.PutUint32(hugeTable[dibTestColorsUsedOffset:], math.MaxUint32)
	cases := []struct {
		name string
		dib  []byte
		want error
	}{
		{"no bytes", nil, errDIBTruncated},
		{"size field cut short", []byte{40, 0}, errDIBTruncated},
		{"BITMAPINFOHEADER cut short", full[:dibInfoHeaderSize-1], errDIBTruncated},
		{"BITMAPV5HEADER cut short before its alpha mask", v5[:dibAlphaMaskOffset], errDIBTruncated},
		{"last pixel row cut short", full[:len(full)-1], errDIBTruncated},
		{"bit-field masks cut short", with(func(f *dibFixture) {
			f.bitCount, f.compression, f.width, f.height, f.pixels = 32, dibCompressionBitfields, 1, 1, nil
			f.masks = dibStandardMasks(0)
		})[:dibInfoHeaderSize+8], errDIBTruncated},
		{"color table past the data", with(func(f *dibFixture) { f.colorsUsed = 4 })[:len(full)+12], errDIBTruncated},
		{"color table count beyond any buffer", hugeTable, errDIBTruncated},
		{"BITMAPCOREHEADER", append(binary.LittleEndian.AppendUint32(nil, 12), make([]byte, 64)...), errDIBUnsupported},
		{"undocumented 56-byte header", with(func(f *dibFixture) { f.headerSize = 56 }), errDIBUnsupported},
		{"8 bits per pixel", with(func(f *dibFixture) { f.bitCount = 8 }), errDIBUnsupported},
		{"16 bits per pixel", with(func(f *dibFixture) { f.bitCount = 16 }), errDIBUnsupported},
		{"RLE8 compression", with(func(f *dibFixture) { f.compression = dibTestCompressionRLE8 }), errDIBUnsupported},
		{"PNG compression", with(func(f *dibFixture) { f.compression = dibTestCompressionPNG }), errDIBUnsupported},
		{"zero width", with(func(f *dibFixture) { f.width = 0 }), errDIBInvalid},
		{"negative width", with(func(f *dibFixture) { f.width = -3 }), errDIBInvalid},
		{"zero height", with(func(f *dibFixture) { f.height = 0 }), errDIBInvalid},
		{"two planes", with(func(f *dibFixture) { f.planes = 2 }), errDIBInvalid},
		{"bit fields on 24-bit pixels", with(func(f *dibFixture) {
			f.compression, f.masks = dibCompressionBitfields, dibStandardMasks(0)
		}), errDIBInvalid},
		{"split red mask", with(func(f *dibFixture) {
			f.bitCount, f.compression, f.width, f.pixels = 32, dibCompressionBitfields, 1, dibDWORDRow(0, 0)
			f.masks = [4]uint32{0x00FF00FF, 0x0000FF00, 0xFF000000}
		}), errDIBInvalid},
		{"overlapping green and blue masks", with(func(f *dibFixture) {
			f.bitCount, f.compression, f.width, f.pixels = 32, dibCompressionBitfields, 1, dibDWORDRow(0, 0)
			f.masks = [4]uint32{0x00FF0000, 0x0000FF00, 0x00000FF0}
		}), errDIBInvalid},
		{"alpha mask over the blue byte", with(func(f *dibFixture) {
			f.headerSize, f.bitCount, f.width, f.pixels = dibV5HeaderSize, 32, 1, dibDWORDRow(0, 0)
			f.masks = dibStandardMasks(0x000000FF)
		}), errDIBInvalid},
		{"empty red mask", with(func(f *dibFixture) {
			f.headerSize, f.bitCount, f.compression, f.width, f.pixels = dibV4HeaderSize, 32, dibCompressionBitfields, 1, dibDWORDRow(0, 0)
			f.masks = [4]uint32{0, 0x0000FF00, 0x000000FF, 0xFF000000}
		}), errDIBInvalid},
		{"one row past 32 MiB of pixels", with(func(f *dibFixture) {
			f.bitCount, f.width, f.height, f.pixels = 32, 2048, transferLimit/8192+1, nil
		}), ErrTransferTooLarge},
		{"exactly 32 MiB of pixels passes the bound and then needs every byte", with(func(f *dibFixture) {
			f.bitCount, f.width, f.height, f.pixels = 32, 2048, transferLimit/8192, nil
		}), errDIBTruncated},
		{"widest declared width", with(func(f *dibFixture) { f.width, f.height = math.MaxInt32, 1 }), ErrTransferTooLarge},
		{"tallest declared top-down height", with(func(f *dibFixture) { f.width, f.height = 1, math.MinInt32 }), ErrTransferTooLarge},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			encoded, err := pngFromDIB(tc.dib)
			if !errors.Is(err, tc.want) {
				t.Fatalf("pngFromDIB error %v, want %v", err, tc.want)
			}
			if encoded != nil {
				t.Fatalf("pngFromDIB returned %d bytes with its error", len(encoded))
			}
		})
	}
}
