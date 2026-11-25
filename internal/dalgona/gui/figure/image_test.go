package figure

import (
	"bytes"
	"compress/zlib"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
	"image"
	"image/color"
	"image/gif"
	"image/jpeg"
	"image/png"
	"math"
	"slices"
	"testing"
)

var imageGarbageIDAT = imagePNGChunk("IDAT", []byte("not a zlib stream"))

func imagePNGChunk(kind string, payload []byte) []byte {
	chunk := binary.BigEndian.AppendUint32(nil, uint32(len(payload))) //nolint:gosec // fixture payloads hold fewer than 2^31 bytes
	chunk = append(chunk, kind...)
	chunk = append(chunk, payload...)
	return binary.BigEndian.AppendUint32(chunk, crc32.ChecksumIEEE(chunk[4:]))
}

func imagePNGHeader(width, height uint32, depth, colorType byte) []byte {
	payload := binary.BigEndian.AppendUint32(nil, width)
	payload = binary.BigEndian.AppendUint32(payload, height)
	return append(payload, depth, colorType, 0, 0, 0)
}

func imagePNGFile(header []byte, chunks ...[]byte) []byte {
	signed := append([]byte("\x89PNG\r\n\x1a\n"), imagePNGChunk("IHDR", header)...)
	return slices.Concat(append([][]byte{signed}, chunks...)...)
}

func imageRawPNG(tb testing.TB, header []byte, rowBytes, rows int, ancillary ...[]byte) []byte {
	tb.Helper()
	var compressed bytes.Buffer
	writer := zlib.NewWriter(&compressed)
	if _, err := writer.Write(make([]byte, (1+rowBytes)*rows)); err != nil {
		tb.Fatal(err)
	}
	if err := writer.Close(); err != nil {
		tb.Fatal(err)
	}
	chunks := append(slices.Clone(ancillary), imagePNGChunk("IDAT", compressed.Bytes()), imagePNGChunk("IEND", nil))
	return imagePNGFile(header, chunks...)
}

func imageEncodedPNG(tb testing.TB, m image.Image) []byte {
	tb.Helper()
	var encoded bytes.Buffer
	if err := png.Encode(&encoded, m); err != nil {
		tb.Fatal(err)
	}
	return encoded.Bytes()
}

func imageEncodedJPEG(tb testing.TB, m image.Image) []byte {
	tb.Helper()
	var encoded bytes.Buffer
	if err := jpeg.Encode(&encoded, m, &jpeg.Options{Quality: 100}); err != nil {
		tb.Fatal(err)
	}
	return encoded.Bytes()
}

func imageEncodedGIF(tb testing.TB, animation *gif.GIF) []byte {
	tb.Helper()
	var encoded bytes.Buffer
	if err := gif.EncodeAll(&encoded, animation); err != nil {
		tb.Fatal(err)
	}
	return encoded.Bytes()
}

func imageJPEGSegment(marker byte, payload []byte) []byte {
	segment := binary.BigEndian.AppendUint16([]byte{0xFF, marker}, uint16(len(payload)+2)) //nolint:gosec // fixture segments hold fewer than 2^16 bytes
	return append(segment, payload...)
}

func imageJPEGFrame(width, height uint16, components ...[3]byte) []byte {
	frame := binary.BigEndian.AppendUint16([]byte{8}, height)
	frame = binary.BigEndian.AppendUint16(frame, width)
	frame = append(frame, byte(len(components))) //nolint:gosec // fixtures declare at most four components
	for _, component := range components {
		frame = append(frame, component[:]...)
	}
	return frame
}

func imageJPEGFile(segments ...[]byte) []byte {
	data := slices.Concat(append([][]byte{{0xFF, 0xD8}}, segments...)...)
	return append(data, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00)
}

var imageGrayComponent = [3]byte{1, 0x11, 0}

type imageExifEntry struct {
	tag, kind uint16
	count     uint32
	value     uint16
}

func imageExifSegment(order binary.AppendByteOrder, ifdOffset uint32, entries ...imageExifEntry) []byte {
	tiff := []byte("II*\x00")
	if order == binary.BigEndian {
		tiff = []byte("MM\x00*")
	}
	tiff = order.AppendUint32(tiff, ifdOffset)
	tiff = order.AppendUint16(tiff, uint16(len(entries))) //nolint:gosec // fixtures hold at most two entries
	for _, entry := range entries {
		tiff = order.AppendUint16(tiff, entry.tag)
		tiff = order.AppendUint16(tiff, entry.kind)
		tiff = order.AppendUint32(tiff, entry.count)
		tiff = order.AppendUint16(tiff, entry.value)
		tiff = order.AppendUint16(tiff, 0)
	}
	tiff = order.AppendUint32(tiff, 0)
	return imageJPEGSegment(0xE1, append([]byte("Exif\x00\x00"), tiff...))
}

func imageOrientationEntry(value uint16) imageExifEntry {
	return imageExifEntry{tag: 0x0112, kind: 3, count: 1, value: value}
}

func imageWithExif(encoded []byte, exif []byte) []byte {
	return slices.Concat(encoded[:2], exif, encoded[2:])
}

func imageGIFFile(width, height uint16, frames ...[4]uint16) []byte {
	data := binary.LittleEndian.AppendUint16([]byte("GIF89a"), width)
	data = binary.LittleEndian.AppendUint16(data, height)
	data = append(data, 0x80, 0, 0, 0, 0, 0, 255, 255, 255)
	for _, frame := range frames {
		data = append(data, 0x2C)
		for _, field := range frame {
			data = binary.LittleEndian.AppendUint16(data, field)
		}
		data = append(data, 0, 2, 2, 0x44, 0x01, 0)
	}
	return append(data, 0x3B)
}

func imageRequireError(tb testing.TB, err, want error) {
	tb.Helper()
	if !errors.Is(err, want) {
		tb.Fatalf("error = %v, want %v", err, want)
	}
}

func imageRequirePixels(tb testing.TB, got *image.NRGBA, bounds image.Rectangle, want func(x, y int) color.Color) {
	tb.Helper()
	if !got.Rect.Eq(bounds) {
		tb.Fatalf("bounds = %v, want %v", got.Rect, bounds)
	}
	for y := bounds.Min.Y; y < bounds.Max.Y; y++ {
		for x := bounds.Min.X; x < bounds.Max.X; x++ {
			if pixel := got.NRGBAAt(x, y); color.Color(pixel) != want(x, y) {
				tb.Fatalf("pixel %d,%d = %v, want %v", x, y, pixel, want(x, y))
			}
		}
	}
}

func imageNativeBytes(tb testing.TB, m image.Image) int64 {
	tb.Helper()
	r := m.Bounds()
	switch m := m.(type) {
	case *image.YCbCr:
		planes := image.NewYCbCr(r, m.SubsampleRatio)
		return int64(len(planes.Y) + len(planes.Cb) + len(planes.Cr))
	case *image.Paletted:
		return int64(len(image.NewPaletted(r, nil).Pix))
	case *image.CMYK:
		return int64(len(image.NewCMYK(r).Pix))
	}
	return imageChannelBytes(tb, m)
}

func imageChannelBytes(tb testing.TB, m image.Image) int64 {
	tb.Helper()
	r := m.Bounds()
	switch m.(type) {
	case *image.Gray:
		return int64(len(image.NewGray(r).Pix))
	case *image.Gray16:
		return int64(len(image.NewGray16(r).Pix))
	case *image.NRGBA:
		return int64(len(image.NewNRGBA(r).Pix))
	case *image.NRGBA64:
		return int64(len(image.NewNRGBA64(r).Pix))
	case *image.RGBA:
		return int64(len(image.NewRGBA(r).Pix))
	case *image.RGBA64:
		return int64(len(image.NewRGBA64(r).Pix))
	}
	tb.Fatalf("unexpected decoded type %T", m)
	return 0
}

func imageStdlibDecode(tb testing.TB, format ImageFormat, data []byte) image.Image {
	tb.Helper()
	decode := png.Decode
	if format == ImageJPEG {
		decode = jpeg.Decode
	}
	decoded, err := decode(bytes.NewReader(data))
	if err != nil {
		tb.Fatal(err)
	}
	return decoded
}

func imageYCbCrBytes(width, height int, ratio image.YCbCrSubsampleRatio) int64 {
	planes := image.NewYCbCr(image.Rect(0, 0, width, height), ratio)
	return int64(len(planes.Y) + len(planes.Cb) + len(planes.Cr))
}

func imageOpaqueRGBA(width, height int) *image.RGBA {
	m := image.NewRGBA(image.Rect(0, 0, width, height))
	var level byte
	for i := range m.Pix {
		level += 29
		m.Pix[i] = level
	}
	for i := 3; i < len(m.Pix); i += 4 {
		m.Pix[i] = 0xFF
	}
	return m
}

func imagePattern(width, height int) *image.NRGBA {
	m := image.NewNRGBA(image.Rect(0, 0, width, height))
	var level byte
	for i := range m.Pix {
		level += 37
		m.Pix[i] = level
	}
	return m
}

func imageGrayLabels(width, height int, labels string) *image.Gray {
	m := image.NewGray(image.Rect(0, 0, width, height))
	for i, label := range []byte(labels) {
		m.Pix[i] = 20 + 40*(label-'a')
	}
	return m
}

func imageLabelRows(m *image.NRGBA) []string {
	var rows []string
	for y := m.Rect.Min.Y; y < m.Rect.Max.Y; y++ {
		var row []byte
		for x := m.Rect.Min.X; x < m.Rect.Max.X; x++ {
			row = append(row, 'a'+m.NRGBAAt(x, y).R/40)
		}
		rows = append(rows, string(row))
	}
	return rows
}

func TestInspectImageRefusesOversizedDimensionsBeforeDecoding(t *testing.T) {
	cases := map[string][]byte{
		"png wider than the side limit with garbage pixel data": imagePNGFile(imagePNGHeader(20000, 10, 8, 6), imageGarbageIDAT, imagePNGChunk("IEND", nil)),
		"png wider than the side limit without pixel data":      imagePNGFile(imagePNGHeader(20000, 10, 8, 6)),
		"png taller than the side limit":                        imagePNGFile(imagePNGHeader(1, 16385, 8, 0), imageGarbageIDAT),
		"png square over the pixel count":                       imagePNGFile(imagePNGHeader(6000, 6000, 8, 2), imageGarbageIDAT),
		"png one row over the pixel count":                      imagePNGFile(imagePNGHeader(8000, 4001, 8, 0), imageGarbageIDAT),
		"jpeg wider than the side limit":                        imageJPEGFile(imageJPEGSegment(0xC0, imageJPEGFrame(20000, 10, imageGrayComponent))),
		"jpeg square over the pixel count":                      imageJPEGFile(imageJPEGSegment(0xC0, imageJPEGFrame(6000, 6000, imageGrayComponent))),
		"gif screen wider than the side limit without frames":   imageGIFFile(20000, 1),
	}
	for name, data := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := InspectImage(data)
			imageRequireError(t, err, ErrImageLimit)
			decoded, _, err := DecodeImage(data)
			imageRequireError(t, err, ErrImageLimit)
			if decoded != nil {
				t.Fatalf("DecodeImage returned pixels for a refused header")
			}
		})
	}
}

func TestInspectImageAcceptsDimensionsAtTheLimits(t *testing.T) {
	cases := []struct {
		name          string
		data          []byte
		format        ImageFormat
		width, height int
		pixelBytes    int64
	}{
		{"png side at the limit", imagePNGFile(imagePNGHeader(16384, 1, 8, 6), imageGarbageIDAT), ImagePNG, 16384, 1, 65_536},
		{"png pixel count at the limit", imagePNGFile(imagePNGHeader(8000, 4000, 8, 0), imageGarbageIDAT), ImagePNG, 8000, 4000, 32_000_000},
		{"png 16-bit RGBA at the pixel byte limit", imagePNGFile(imagePNGHeader(4096, 4096, 16, 6), imageGarbageIDAT), ImagePNG, 4096, 4096, 134_217_728},
		{"png 8-bit RGBA at 4100 square", imagePNGFile(imagePNGHeader(4100, 4100, 8, 6), imageGarbageIDAT), ImagePNG, 4100, 4100, 67_240_000},
		{"png 16-bit gray without transparency", imagePNGFile(imagePNGHeader(4100, 4100, 16, 0), imageGarbageIDAT), ImagePNG, 4100, 4100, 33_620_000},
		{"jpeg side at the limit", imageJPEGFile(imageJPEGSegment(0xC0, imageJPEGFrame(16384, 1, imageGrayComponent))), ImageJPEG, 16384, 1, 16_384},
		{"gif side at the limit", imageGIFFile(16384, 1, [4]uint16{0, 0, 1, 1}), ImageGIF, 16384, 1, 16_384},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			header, err := InspectImage(tc.data)
			if err != nil {
				t.Fatal(err)
			}
			want := ImageHeader{Format: tc.format, Width: tc.width, Height: tc.height, Frames: 1, PixelBytes: tc.pixelBytes, Orientation: 1}
			if header != want {
				t.Fatalf("header = %+v, want %+v", header, want)
			}
		})
	}
}

func TestInspectImageRefusesPixelBytesAboveLimit(t *testing.T) {
	transparentGray := imagePNGChunk("tRNS", []byte{0, 0})
	cases := map[string][]byte{
		"16-bit RGBA at 4100 square":           imagePNGFile(imagePNGHeader(4100, 4100, 16, 6), imageGarbageIDAT),
		"16-bit RGBA one row over the limit":   imagePNGFile(imagePNGHeader(4096, 4097, 16, 6), imageGarbageIDAT),
		"16-bit RGB at 4100 square":            imagePNGFile(imagePNGHeader(4100, 4100, 16, 2), imageGarbageIDAT),
		"16-bit gray alpha at 4100 square":     imagePNGFile(imagePNGHeader(4100, 4100, 16, 4), imageGarbageIDAT),
		"16-bit gray with transparency key":    imagePNGFile(imagePNGHeader(4100, 4100, 16, 0), transparentGray, imageGarbageIDAT),
		"16-bit gray with transparency second": imagePNGFile(imagePNGHeader(4100, 4100, 16, 0), imagePNGChunk("gAMA", []byte{0, 0, 177, 143}), transparentGray, imageGarbageIDAT),
	}
	for name, data := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := InspectImage(data)
			imageRequireError(t, err, ErrImageLimit)
			_, _, err = DecodeImage(data)
			imageRequireError(t, err, ErrImageLimit)
		})
	}
}

func TestInspectImageRefusesEncodedSizeAboveLimit(t *testing.T) {
	small := imageEncodedPNG(t, imageOpaqueRGBA(2, 2))
	atLimit := append(slices.Clone(small), make([]byte, ImageEncodedLimit-len(small))...)
	if _, err := InspectImage(atLimit); err != nil {
		t.Fatalf("InspectImage at the encoded limit: %v", err)
	}
	_, err := InspectImage(append(atLimit, 0))
	imageRequireError(t, err, ErrImageLimit)
}

func TestImageProductSaturatesOnOverflow(t *testing.T) {
	cases := []struct{ a, b, want int64 }{
		{0, math.MaxInt64, 0},
		{16384, 16384, 268_435_456},
		{1 << 32, 1 << 31, math.MaxInt64},
		{math.MaxInt64, 2, math.MaxInt64},
		{3_037_000_500, 3_037_000_500, math.MaxInt64},
	}
	for _, tc := range cases {
		if got := imageProduct(tc.a, tc.b); got != tc.want {
			t.Fatalf("imageProduct(%d, %d) = %d, want %d", tc.a, tc.b, got, tc.want)
		}
	}
}

func TestInspectImagePixelBytesMatchStdlibDecoder(t *testing.T) {
	translucent := image.NewNRGBA(image.Rect(0, 0, 5, 3))
	translucent.SetNRGBA(1, 1, color.NRGBA{10, 20, 30, 128})
	wide := image.NewNRGBA64(image.Rect(0, 0, 5, 3))
	wide.SetNRGBA64(2, 1, color.NRGBA64{1000, 2000, 3000, 30000})
	deep := image.NewRGBA64(image.Rect(0, 0, 5, 3))
	for i := range deep.Pix {
		deep.Pix[i] = 0xFF
	}
	twoColors := image.NewPaletted(image.Rect(0, 0, 5, 3), color.Palette{color.Black, color.White})
	cases := map[string][]byte{
		"png gray 8":                    imageEncodedPNG(t, image.NewGray(image.Rect(0, 0, 5, 3))),
		"png gray 16":                   imageEncodedPNG(t, image.NewGray16(image.Rect(0, 0, 5, 3))),
		"png gray 1-bit":                imageRawPNG(t, imagePNGHeader(5, 3, 1, 0), 1, 3),
		"png gray 8 with transparency":  imageRawPNG(t, imagePNGHeader(5, 3, 8, 0), 5, 3, imagePNGChunk("tRNS", []byte{0, 0})),
		"png gray 16 with transparency": imageRawPNG(t, imagePNGHeader(5, 3, 16, 0), 10, 3, imagePNGChunk("tRNS", []byte{0, 0})),
		"png gray alpha 8":              imageRawPNG(t, imagePNGHeader(5, 3, 8, 4), 10, 3),
		"png gray alpha 16":             imageRawPNG(t, imagePNGHeader(5, 3, 16, 4), 20, 3),
		"png rgb 8":                     imageEncodedPNG(t, imageOpaqueRGBA(5, 3)),
		"png rgb 8 with transparency":   imageRawPNG(t, imagePNGHeader(5, 3, 8, 2), 15, 3, imagePNGChunk("tRNS", []byte{0, 0, 0, 0, 0, 0})),
		"png rgba 8":                    imageEncodedPNG(t, translucent),
		"png rgb 16":                    imageEncodedPNG(t, deep),
		"png rgba 16":                   imageEncodedPNG(t, wide),
		"png paletted 1-bit":            imageEncodedPNG(t, twoColors),
		"jpeg gray":                     imageEncodedJPEG(t, image.NewGray(image.Rect(0, 0, 17, 9))),
		"jpeg color":                    imageEncodedJPEG(t, imageOpaqueRGBA(17, 9)),
	}
	for name, data := range cases {
		t.Run(name, func(t *testing.T) {
			header, err := InspectImage(data)
			if err != nil {
				t.Fatal(err)
			}
			decoded := imageStdlibDecode(t, header.Format, data)
			if want := imageNativeBytes(t, decoded); header.PixelBytes != want {
				t.Fatalf("PixelBytes = %d, want %d for decoded %T", header.PixelBytes, want, decoded)
			}
		})
	}
}

func TestInspectImageJPEGPixelBytesFollowDecoderLayout(t *testing.T) {
	const width, height = 17, 9
	frame := func(components ...[3]byte) []byte {
		return imageJPEGSegment(0xC0, imageJPEGFrame(width, height, components...))
	}
	ycbcr := func(luma, blue, red byte) []byte {
		return frame([3]byte{1, luma, 0}, [3]byte{2, blue, 1}, [3]byte{3, red, 1})
	}
	rgbIDs := frame([3]byte{'R', 0x11, 0}, [3]byte{'G', 0x11, 0}, [3]byte{'B', 0x11, 0})
	adobe := func(transform byte) []byte {
		return imageJPEGSegment(0xEE, append([]byte("Adobe"), 0, 100, 0, 0, 0, 0, transform))
	}
	jfif := imageJPEGSegment(0xE0, []byte("JFIF\x00\x01\x02\x00\x00\x01\x00\x01\x00\x00"))
	fourBytes := int64(width * height * 4)
	full := imageYCbCrBytes(width, height, image.YCbCrSubsampleRatio444)
	cases := []struct {
		name     string
		segments [][]byte
		want     int64
	}{
		{"4:4:4", [][]byte{ycbcr(0x11, 0x11, 0x11)}, full},
		{"4:2:2", [][]byte{ycbcr(0x21, 0x11, 0x11)}, imageYCbCrBytes(width, height, image.YCbCrSubsampleRatio422)},
		{"4:2:0", [][]byte{ycbcr(0x22, 0x11, 0x11)}, imageYCbCrBytes(width, height, image.YCbCrSubsampleRatio420)},
		{"4:4:0", [][]byte{ycbcr(0x12, 0x11, 0x11)}, imageYCbCrBytes(width, height, image.YCbCrSubsampleRatio440)},
		{"4:1:1", [][]byte{ycbcr(0x41, 0x11, 0x11)}, imageYCbCrBytes(width, height, image.YCbCrSubsampleRatio411)},
		{"4:1:0", [][]byte{ycbcr(0x42, 0x11, 0x11)}, imageYCbCrBytes(width, height, image.YCbCrSubsampleRatio410)},
		{"luma below the chroma maximum", [][]byte{ycbcr(0x11, 0x22, 0x22)}, full},
		{"unequal chroma factors", [][]byte{ycbcr(0x22, 0x21, 0x11)}, full},
		{"vertical ratio four", [][]byte{ycbcr(0x14, 0x11, 0x11)}, full},
		{"gray", [][]byte{frame(imageGrayComponent)}, width * height},
		{"cmyk", [][]byte{adobe(0), frame([3]byte{1, 0x11, 0}, [3]byte{2, 0x11, 0}, [3]byte{3, 0x11, 0}, [3]byte{4, 0x11, 0})}, fourBytes},
		{"adobe rgb transform", [][]byte{adobe(0), ycbcr(0x11, 0x11, 0x11)}, fourBytes},
		{"adobe ycbcr transform", [][]byte{adobe(1), ycbcr(0x11, 0x11, 0x11)}, full},
		{"rgb component identifiers", [][]byte{rgbIDs}, fourBytes},
		{"jfif overrides rgb identifiers", [][]byte{jfif, rgbIDs}, full},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			header, err := InspectImage(imageJPEGFile(tc.segments...))
			if err != nil {
				t.Fatal(err)
			}
			if header.PixelBytes != tc.want || header.Width != width || header.Height != height {
				t.Fatalf("header = %+v, want %dx%d with %d pixel bytes", header, width, height, tc.want)
			}
		})
	}
}

func TestDecodeImageKeepsTranslucentPNGPixels(t *testing.T) {
	source := imagePattern(3, 2)
	decoded, header, err := DecodeImage(imageEncodedPNG(t, source))
	if err != nil {
		t.Fatal(err)
	}
	if header.Format != ImagePNG {
		t.Fatalf("Format = %v, want png", header.Format)
	}
	imageRequirePixels(t, decoded, source.Rect, func(x, y int) color.Color { return source.NRGBAAt(x, y) })
}

func TestDecodeImageConvertsOpaquePNGPixels(t *testing.T) {
	source := imageOpaqueRGBA(3, 2)
	decoded, _, err := DecodeImage(imageEncodedPNG(t, source))
	if err != nil {
		t.Fatal(err)
	}
	imageRequirePixels(t, decoded, source.Rect, func(x, y int) color.Color { return color.NRGBA(source.RGBAAt(x, y)) })
}

func TestDecodeImageMatchesTheStdlibJPEGDecoder(t *testing.T) {
	data := imageEncodedJPEG(t, imageOpaqueRGBA(17, 9))
	decoded, header, err := DecodeImage(data)
	if err != nil {
		t.Fatal(err)
	}
	if want := (ImageHeader{Format: ImageJPEG, Width: 17, Height: 9, Frames: 1, PixelBytes: imageYCbCrBytes(17, 9, image.YCbCrSubsampleRatio420), Orientation: 1}); header != want {
		t.Fatalf("header = %+v, want %+v", header, want)
	}
	oracle := imageStdlibDecode(t, ImageJPEG, data)
	imageRequirePixels(t, decoded, oracle.Bounds(), func(x, y int) color.Color { return color.NRGBAModel.Convert(oracle.At(x, y)) })
}

func TestDecodeImageReturnsTheFirstGIFFrame(t *testing.T) {
	palettes := []color.Palette{
		{color.NRGBA{255, 0, 0, 255}, color.NRGBA{0, 0, 255, 255}},
		{color.NRGBA{0, 255, 0, 255}, color.NRGBA{9, 9, 9, 255}},
		{color.NRGBA{1, 2, 3, 255}, color.NRGBA{4, 5, 6, 255}},
	}
	animation := &gif.GIF{LoopCount: 2}
	for _, palette := range palettes {
		frame := image.NewPaletted(image.Rect(0, 0, 4, 3), palette)
		frame.SetColorIndex(1, 2, 1)
		animation.Image = append(animation.Image, frame)
		animation.Delay = append(animation.Delay, 10)
		animation.Disposal = append(animation.Disposal, gif.DisposalBackground)
	}
	decoded, header, err := DecodeImage(imageEncodedGIF(t, animation))
	if err != nil {
		t.Fatal(err)
	}
	if want := (ImageHeader{Format: ImageGIF, Width: 4, Height: 3, Frames: 3, PixelBytes: 12, Orientation: 1}); header != want {
		t.Fatalf("header = %+v, want %+v", header, want)
	}
	first := animation.Image[0]
	imageRequirePixels(t, decoded, image.Rect(0, 0, 4, 3), func(x, y int) color.Color { return first.At(x, y) })
}

func TestDecodeImagePlacesASmallGIFFrameOnItsScreen(t *testing.T) {
	palette := color.Palette{color.NRGBA{200, 100, 50, 255}, color.NRGBA{0, 0, 0, 255}}
	frame := image.NewPaletted(image.Rect(1, 1, 3, 2), palette)
	animation := &gif.GIF{Image: []*image.Paletted{frame}, Delay: []int{0}, Config: image.Config{ColorModel: palette, Width: 4, Height: 3}}
	decoded, header, err := DecodeImage(imageEncodedGIF(t, animation))
	if err != nil {
		t.Fatal(err)
	}
	if header.Width != 4 || header.Height != 3 {
		t.Fatalf("header = %+v, want the 4x3 screen", header)
	}
	imageRequirePixels(t, decoded, image.Rect(0, 0, 4, 3), func(x, y int) color.Color {
		if image.Pt(x, y).In(frame.Rect) {
			return palette[0]
		}
		return color.NRGBA{}
	})
}

func TestInspectImageIdentifiesFormatFromMagicBytesOnly(t *testing.T) {
	pngData := imageEncodedPNG(t, imageOpaqueRGBA(2, 2))
	jpegData := imageEncodedJPEG(t, imageOpaqueRGBA(2, 2))
	gifData := imageEncodedGIF(t, &gif.GIF{Image: []*image.Paletted{image.NewPaletted(image.Rect(0, 0, 2, 2), color.Palette{color.Black, color.White})}, Delay: []int{0}})
	admission := AdmitImages(nil, []ImageCandidate{
		{Name: "photo.jpg", Data: pngData},
		{Name: "scan.png", Data: jpegData},
		{Name: "clip.jpeg", Data: gifData},
		{Name: "notes.png", Data: []byte("plain text that only claims to be an image")},
	})
	imageRequireAdmission(t, admission, []string{"photo.jpg", "scan.png", "clip.jpeg"}, ImageFailure{Name: "notes.png", Err: ErrImageFormat})
	formats := make([]ImageFormat, len(admission.Headers))
	for i, header := range admission.Headers {
		formats[i] = header.Format
	}
	if want := []ImageFormat{ImagePNG, ImageJPEG, ImageGIF}; !slices.Equal(formats, want) {
		t.Fatalf("formats = %v, want %v", formats, want)
	}
	for _, prefix := range []string{"", "\x89PNG\r\n\x1a", "\xFF\xD8", "GIF8", "GIF90a\x01\x00\x01\x00\x00\x00\x00;", "BM\x00\x00\x00\x00\x00\x00"} {
		_, err := InspectImage([]byte(prefix))
		imageRequireError(t, err, ErrImageFormat)
	}
}

func TestImageFormatNames(t *testing.T) {
	cases := []struct {
		format          ImageFormat
		name, mediaType string
	}{
		{ImagePNG, "png", "image/png"},
		{ImageJPEG, "jpeg", "image/jpeg"},
		{ImageGIF, "gif", "image/gif"},
	}
	for _, tc := range cases {
		if tc.format.String() != tc.name || tc.format.MediaType() != tc.mediaType {
			t.Fatalf("format %d names %q %q, want %q %q", uint8(tc.format), tc.format.String(), tc.format.MediaType(), tc.name, tc.mediaType)
		}
	}
}

func imageRequireTruncationRefused(t *testing.T, data []byte, from, headerEnd int) {
	t.Helper()
	for n := from; n < headerEnd; n++ {
		_, err := InspectImage(data[:n])
		imageRequireError(t, err, ErrImageHeader)
	}
	_, _, err := DecodeImage(data[:headerEnd-1])
	imageRequireError(t, err, ErrImageHeader)
	if _, err := InspectImage(data[:headerEnd]); err != nil {
		t.Fatalf("InspectImage with the complete header: %v", err)
	}
}

func TestInspectImageRefusesTruncatedPNGHeaders(t *testing.T) {
	data := imageEncodedPNG(t, imageOpaqueRGBA(16, 16))
	imageRequireTruncationRefused(t, data, 8, bytes.Index(data, []byte("IDAT"))+4)
}

func TestInspectImageRefusesTruncatedJPEGHeaders(t *testing.T) {
	data := imageEncodedJPEG(t, imageOpaqueRGBA(16, 16))
	frame := bytes.Index(data, []byte{0xFF, 0xC0})
	imageRequireTruncationRefused(t, data, 3, frame+2+int(binary.BigEndian.Uint16(data[frame+2:])))
}

func TestInspectImageRefusesTruncatedGIFStreams(t *testing.T) {
	palette := color.Palette{color.Black, color.White}
	animation := &gif.GIF{LoopCount: 1, Image: []*image.Paletted{image.NewPaletted(image.Rect(0, 0, 3, 3), palette), image.NewPaletted(image.Rect(0, 0, 3, 3), palette)}, Delay: []int{5, 5}}
	data := imageEncodedGIF(t, animation)
	imageRequireTruncationRefused(t, data, 6, len(data))
}

func TestInspectImageRefusesMalformedPNGHeaders(t *testing.T) {
	header := func(edit func([]byte)) []byte {
		payload := imagePNGHeader(4, 4, 8, 6)
		edit(payload)
		return imagePNGFile(payload, imageGarbageIDAT)
	}
	badCRC := imagePNGFile(imagePNGHeader(4, 4, 8, 6), imageGarbageIDAT)
	badCRC[29] ^= 0xFF
	signature := []byte("\x89PNG\r\n\x1a\n")
	cases := map[string][]byte{
		"header checksum mismatch":       badCRC,
		"first chunk is not the header":  slices.Concat(signature, imagePNGChunk("gAMA", []byte{0, 0, 177, 143}), imagePNGChunk("IHDR", imagePNGHeader(4, 4, 8, 6)), imageGarbageIDAT),
		"header shorter than 13 bytes":   slices.Concat(signature, imagePNGChunk("IHDR", imagePNGHeader(4, 4, 8, 6)[:12]), imageGarbageIDAT),
		"4-bit truecolor":                header(func(p []byte) { p[8], p[9] = 4, 2 }),
		"16-bit palette":                 header(func(p []byte) { p[8], p[9] = 16, 3 }),
		"3-bit gray":                     header(func(p []byte) { p[8], p[9] = 3, 0 }),
		"color type 1":                   header(func(p []byte) { p[8], p[9] = 8, 1 }),
		"compression method 1":           header(func(p []byte) { p[10] = 1 }),
		"filter method 1":                header(func(p []byte) { p[11] = 1 }),
		"interlace method 2":             header(func(p []byte) { p[12] = 2 }),
		"zero width":                     imagePNGFile(imagePNGHeader(0, 4, 8, 6), imageGarbageIDAT),
		"zero height":                    imagePNGFile(imagePNGHeader(4, 0, 8, 6), imageGarbageIDAT),
		"width beyond the PNG range":     imagePNGFile(imagePNGHeader(1<<31, 4, 8, 6), imageGarbageIDAT),
		"end chunk before pixel data":    imagePNGFile(imagePNGHeader(4, 4, 8, 6), imagePNGChunk("IEND", nil), imageGarbageIDAT),
		"chunk longer than the file":     imagePNGFile(imagePNGHeader(4, 4, 8, 6), imagePNGChunk("tEXt", []byte("abc"))[:9]),
		"chunk length beyond 31 bits":    imagePNGFile(imagePNGHeader(4, 4, 8, 6), []byte{0xFF, 0xFF, 0xFF, 0xF8, 't', 'E', 'X', 't'}, imageGarbageIDAT),
		"header without following chunk": imagePNGFile(imagePNGHeader(4, 4, 8, 6)),
	}
	for name, data := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := InspectImage(data)
			imageRequireError(t, err, ErrImageHeader)
		})
	}
}

func TestInspectImageSkipsAncillaryPNGChunks(t *testing.T) {
	interlaced := imagePNGHeader(4, 3, 8, 2)
	interlaced[12] = 1
	data := imagePNGFile(interlaced, imagePNGChunk("gAMA", []byte{0, 0, 177, 143}), imagePNGChunk("tEXt", []byte("Title\x00x")), imageGarbageIDAT)
	header, err := InspectImage(data)
	if err != nil {
		t.Fatal(err)
	}
	if want := (ImageHeader{Format: ImagePNG, Width: 4, Height: 3, Frames: 1, PixelBytes: 48, Orientation: 1}); header != want {
		t.Fatalf("header = %+v, want %+v", header, want)
	}
}

func TestInspectImageRefusesMalformedJPEGHeaders(t *testing.T) {
	frame := func(width, height uint16, components ...[3]byte) []byte {
		return imageJPEGSegment(0xC0, imageJPEGFrame(width, height, components...))
	}
	color3 := func(luma byte) []byte {
		return frame(8, 8, [3]byte{1, luma, 0}, [3]byte{2, 0x11, 0}, [3]byte{3, 0x11, 0})
	}
	twelveBit := frame(8, 8, imageGrayComponent)
	twelveBit[4] = 12
	shortFrame := frame(8, 8, imageGrayComponent, imageGrayComponent, imageGrayComponent)
	shortFrame[3] -= 3
	shortFrame = shortFrame[:len(shortFrame)-3]
	cases := []struct {
		name string
		data []byte
		want error
	}{
		{"scan before frame", imageJPEGFile(), ErrImageHeader},
		{"end before frame", []byte{0xFF, 0xD8, 0xFF, 0xD9}, ErrImageHeader},
		{"segment length below two", slices.Concat([]byte{0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x01}, frame(8, 8, imageGrayComponent)), ErrImageHeader},
		{"segment beyond the file", []byte{0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x40, 1, 2, 3}, ErrImageHeader},
		{"frame shorter than its fixed fields", imageJPEGFile(imageJPEGSegment(0xC0, []byte{8, 0, 8, 0})), ErrImageHeader},
		{"frame length disagrees with components", imageJPEGFile(shortFrame), ErrImageHeader},
		{"zero width", imageJPEGFile(frame(0, 8, imageGrayComponent)), ErrImageHeader},
		{"zero height", imageJPEGFile(frame(8, 0, imageGrayComponent)), ErrImageHeader},
		{"sampling factor zero", imageJPEGFile(color3(0x01)), ErrImageHeader},
		{"sampling factor five", imageJPEGFile(color3(0x51)), ErrImageHeader},
		{"sampling factor three", imageJPEGFile(color3(0x31)), ErrImageFormat},
		{"lossless coding", imageJPEGFile(imageJPEGSegment(0xC3, imageJPEGFrame(8, 8, imageGrayComponent))), ErrImageFormat},
		{"arithmetic coding", imageJPEGFile(imageJPEGSegment(0xC9, imageJPEGFrame(8, 8, imageGrayComponent))), ErrImageFormat},
		{"12-bit precision", imageJPEGFile(twelveBit), ErrImageFormat},
		{"two components", imageJPEGFile(frame(8, 8, imageGrayComponent, [3]byte{2, 0x11, 0})), ErrImageFormat},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := InspectImage(tc.data)
			imageRequireError(t, err, tc.want)
		})
	}
}

func TestInspectImageSkipsNonFrameJPEGMarkers(t *testing.T) {
	data := slices.Concat(
		[]byte{0xFF, 0xD8},
		imageJPEGSegment(0xC4, []byte{0, 1, 2}),
		[]byte{0x00, 0x13, 0xFF, 0xFF},
		imageJPEGSegment(0xC8, []byte{3}),
		[]byte{0xFF, 0x00, 0xFF, 0xD0},
		imageJPEGSegment(0xCC, []byte{4, 5}),
		imageJPEGSegment(0xC1, imageJPEGFrame(30, 20, imageGrayComponent)),
	)
	header, err := InspectImage(data)
	if err != nil {
		t.Fatal(err)
	}
	if want := (ImageHeader{Format: ImageJPEG, Width: 30, Height: 20, Frames: 1, PixelBytes: 600, Orientation: 1}); header != want {
		t.Fatalf("header = %+v, want %+v", header, want)
	}
}

func TestInspectImageRefusesMalformedGIFStreams(t *testing.T) {
	unknownBlock := imageGIFFile(4, 4, [4]uint16{0, 0, 1, 1})
	unknownBlock = slices.Insert(unknownBlock, len(unknownBlock)-1, 0x00)
	cases := map[string][]byte{
		"zero screen width":            imageGIFFile(0, 4, [4]uint16{}),
		"zero screen height":           imageGIFFile(4, 0, [4]uint16{}),
		"first frame right of screen":  imageGIFFile(4, 4, [4]uint16{2, 0, 3, 1}),
		"third frame below the screen": imageGIFFile(4, 4, [4]uint16{0, 0, 4, 4}, [4]uint16{3, 3, 1, 1}, [4]uint16{0, 3, 1, 2}),
		"frame at the far coordinate":  imageGIFFile(4, 4, [4]uint16{65535, 0, 1, 1}),
		"unknown block":                unknownBlock,
		"trailer without frames":       imageGIFFile(4, 4),
	}
	for name, data := range cases {
		t.Run(name, func(t *testing.T) {
			_, err := InspectImage(data)
			imageRequireError(t, err, ErrImageHeader)
		})
	}
}

func TestInspectImageCountsGIFFramesWithinTheScreen(t *testing.T) {
	header, err := InspectImage(imageGIFFile(4, 4, [4]uint16{0, 0, 4, 4}, [4]uint16{3, 3, 1, 1}, [4]uint16{1, 0, 2, 4}))
	if err != nil {
		t.Fatal(err)
	}
	if want := (ImageHeader{Format: ImageGIF, Width: 4, Height: 4, Frames: 3, PixelBytes: 16, Orientation: 1}); header != want {
		t.Fatalf("header = %+v, want %+v", header, want)
	}
}

func TestInspectImageReadsJPEGExifOrientation(t *testing.T) {
	base := imageEncodedJPEG(t, image.NewGray(image.Rect(0, 0, 8, 8)))
	truncatedEntry := imageExifSegment(binary.LittleEndian, 8, imageOrientationEntry(6))
	binary.BigEndian.PutUint16(truncatedEntry[2:], binary.BigEndian.Uint16(truncatedEntry[2:])-10)
	truncatedEntry = truncatedEntry[:len(truncatedEntry)-10]
	cases := []struct {
		name string
		exif []byte
		want int
	}{
		{"little endian six", imageExifSegment(binary.LittleEndian, 8, imageOrientationEntry(6)), 6},
		{"big endian three", imageExifSegment(binary.BigEndian, 8, imageOrientationEntry(3)), 3},
		{"big endian eight", imageExifSegment(binary.BigEndian, 8, imageOrientationEntry(8)), 8},
		{"orientation after another tag", imageExifSegment(binary.LittleEndian, 8, imageExifEntry{tag: 0x010F, kind: 2, count: 2, value: 0x41}, imageOrientationEntry(5)), 5},
		{"orientation zero", imageExifSegment(binary.LittleEndian, 8, imageOrientationEntry(0)), 1},
		{"orientation nine", imageExifSegment(binary.LittleEndian, 8, imageOrientationEntry(9)), 1},
		{"orientation stored as long", imageExifSegment(binary.LittleEndian, 8, imageExifEntry{tag: 0x0112, kind: 4, count: 1, value: 6}), 1},
		{"directory offset beyond the segment", imageExifSegment(binary.LittleEndian, 4000, imageOrientationEntry(6)), 1},
		{"directory offset at the maximum", imageExifSegment(binary.BigEndian, math.MaxUint32, imageOrientationEntry(6)), 1},
		{"directory entry cut short", truncatedEntry, 1},
		{"segment without exif prefix", imageJPEGSegment(0xE1, []byte("http://ns.adobe.com/xap/1.0/\x00")), 1},
		{"exif before an xmp segment", slices.Concat(imageExifSegment(binary.LittleEndian, 8, imageOrientationEntry(6)), imageJPEGSegment(0xE1, []byte("http://ns.adobe.com/xap/1.0/\x00"))), 6},
		{"first of two exif segments", slices.Concat(imageExifSegment(binary.BigEndian, 8, imageOrientationEntry(6)), imageExifSegment(binary.BigEndian, 8, imageOrientationEntry(3))), 6},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			header, err := InspectImage(imageWithExif(base, tc.exif))
			if err != nil {
				t.Fatal(err)
			}
			if header.Orientation != tc.want {
				t.Fatalf("Orientation = %d, want %d", header.Orientation, tc.want)
			}
		})
	}
}

func TestDecodeImageRotatesJPEGWithOrientationSix(t *testing.T) {
	stored := image.NewGray(image.Rect(0, 0, 2, 1))
	stored.Pix[1] = 255
	data := imageWithExif(imageEncodedJPEG(t, stored), imageExifSegment(binary.LittleEndian, 8, imageOrientationEntry(6)))
	decoded, header, err := DecodeImage(data)
	if err != nil {
		t.Fatal(err)
	}
	if want := (ImageHeader{Format: ImageJPEG, Width: 2, Height: 1, Frames: 1, PixelBytes: 2, Orientation: 6}); header != want {
		t.Fatalf("header = %+v, want %+v", header, want)
	}
	if !decoded.Rect.Eq(image.Rect(0, 0, 1, 2)) {
		t.Fatalf("bounds = %v, want 1x2", decoded.Rect)
	}
	if top, bottom := decoded.NRGBAAt(0, 0), decoded.NRGBAAt(0, 1); top.R > 64 || bottom.R < 192 {
		t.Fatalf("top %v bottom %v, want dark above light", top, bottom)
	}
}

func TestDecodeImageAppliesEveryJPEGOrientation(t *testing.T) {
	base := imageEncodedJPEG(t, imageGrayLabels(3, 2, "abcdef"))
	want := map[uint16][]string{
		1: {"abc", "def"},
		2: {"cba", "fed"},
		3: {"fed", "cba"},
		4: {"def", "abc"},
		5: {"ad", "be", "cf"},
		6: {"da", "eb", "fc"},
		7: {"fc", "eb", "da"},
		8: {"cf", "be", "ad"},
	}
	for orientation, rows := range want {
		t.Run(fmt.Sprint(orientation), func(t *testing.T) {
			decoded, _, err := DecodeImage(imageWithExif(base, imageExifSegment(binary.BigEndian, 8, imageOrientationEntry(orientation))))
			if err != nil {
				t.Fatal(err)
			}
			if got := imageLabelRows(decoded); !slices.Equal(got, rows) {
				t.Fatalf("rows = %q, want %q", got, rows)
			}
		})
	}
}

func TestImageTileCopiesTheIntersection(t *testing.T) {
	source := imagePattern(300, 300)
	tile, err := ImageTile(source, image.Rect(250, 10, 350, 20))
	if err != nil {
		t.Fatal(err)
	}
	imageRequirePixels(t, tile, image.Rect(250, 10, 300, 20), func(x, y int) color.Color { return source.NRGBAAt(x, y) })
	tile.Pix[0] ^= 0xFF
	if tile.NRGBAAt(250, 10) == source.NRGBAAt(250, 10) {
		t.Fatalf("tile shares pixel storage with the source")
	}
}

func TestImageTileAcceptsTheLargestTileAndEmptyRequests(t *testing.T) {
	source := imagePattern(300, 300)
	full, err := ImageTile(source, image.Rect(44, 44, 300, 300))
	if err != nil {
		t.Fatal(err)
	}
	imageRequirePixels(t, full, image.Rect(44, 44, 300, 300), func(x, y int) color.Color { return source.NRGBAAt(x, y) })
	outside, err := ImageTile(source, image.Rect(400, 400, 500, 500))
	if err != nil || !outside.Rect.Empty() {
		t.Fatalf("tile outside the source = %v, %v", outside.Rect, err)
	}
}

func TestImageTileRefusesOversizedRequests(t *testing.T) {
	source := imagePattern(300, 300)
	for _, r := range []image.Rectangle{
		image.Rect(0, 0, 257, 1),
		image.Rect(0, 0, 1, 257),
		image.Rect(-100, 0, 157, 1),
		{Min: image.Pt(math.MinInt, 0), Max: image.Pt(math.MaxInt, 1)},
		{Min: image.Pt(0, math.MinInt+1), Max: image.Pt(1, math.MaxInt)},
	} {
		_, err := ImageTile(source, r)
		imageRequireError(t, err, ErrImageLimit)
	}
}

func TestImagePreviewAveragesAWideStrip(t *testing.T) {
	source := image.NewNRGBA(image.Rect(0, 0, 1000, 10))
	var want []color.Color
	for block := range byte(100) {
		level := 2 * block
		for x := int(block) * 10; x < int(block)*10+10; x++ {
			for y := 0; y < 10; y += 2 {
				source.SetNRGBA(x, y, color.NRGBA{level, level, level, 255})
				source.SetNRGBA(x, y+1, color.NRGBA{0, 0, 0, 255})
			}
		}
		want = append(want, color.NRGBA{block, block, block, 255})
	}
	imageRequirePixels(t, ImagePreview(source, 100), image.Rect(0, 0, 100, 1), func(x, _ int) color.Color { return want[x] })
}

func TestImagePreviewKeepsTheAspectOfATallStrip(t *testing.T) {
	if preview := ImagePreview(image.NewNRGBA(image.Rect(0, 0, 10, 1000)), 100); !preview.Rect.Eq(image.Rect(0, 0, 1, 100)) {
		t.Fatalf("bounds = %v, want 1x100", preview.Rect)
	}
}

func TestImagePreviewWeighsFractionalCoverage(t *testing.T) {
	source := image.NewNRGBA(image.Rect(0, 0, 3, 1))
	for x, level := range []uint8{0, 90, 180} {
		source.SetNRGBA(x, 0, color.NRGBA{level, level, level, 255})
	}
	want := []color.Color{color.NRGBA{30, 30, 30, 255}, color.NRGBA{150, 150, 150, 255}}
	imageRequirePixels(t, ImagePreview(source, 2), image.Rect(0, 0, 2, 1), func(x, _ int) color.Color { return want[x] })
}

func TestImagePreviewWeighsColorByAlpha(t *testing.T) {
	source := image.NewNRGBA(image.Rect(0, 0, 2, 1))
	source.SetNRGBA(0, 0, color.NRGBA{0, 0, 255, 255})
	source.SetNRGBA(1, 0, color.NRGBA{200, 255, 100, 0})
	imageRequirePixels(t, ImagePreview(source, 1), image.Rect(0, 0, 1, 1), func(int, int) color.Color { return color.NRGBA{0, 0, 255, 128} })
}

func TestImagePreviewReadsTheSubImageOrigin(t *testing.T) {
	source := image.NewNRGBA(image.Rect(0, 0, 4, 2))
	for x, level := range []uint8{250, 250, 10, 30} {
		source.SetNRGBA(x, 0, color.NRGBA{level, level, level, 255})
		source.SetNRGBA(x, 1, color.NRGBA{level, level, level, 255})
	}
	part := source.SubImage(image.Rect(2, 1, 4, 2)).(*image.NRGBA)
	imageRequirePixels(t, ImagePreview(part, 1), image.Rect(0, 0, 1, 1), func(int, int) color.Color { return color.NRGBA{20, 20, 20, 255} })
}

func TestImagePreviewCopiesASmallImage(t *testing.T) {
	source := imagePattern(4, 3)
	preview := ImagePreview(source, 10)
	imageRequirePixels(t, preview, image.Rect(0, 0, 4, 3), func(x, y int) color.Color { return source.NRGBAAt(x, y) })
	preview.Pix[0] ^= 0xFF
	if preview.Pix[0] == source.Pix[0] {
		t.Fatalf("preview shares pixel storage with the source")
	}
}

func imageSizedPNG(tb testing.TB, size int) []byte {
	tb.Helper()
	data := imageEncodedPNG(tb, imageOpaqueRGBA(2, 2))
	return append(data, make([]byte, size-len(data))...)
}

func imageRequireAdmission(tb testing.TB, admission ImageAdmission, accepted []string, failures ...ImageFailure) {
	tb.Helper()
	names := make([]string, len(admission.Accepted))
	for i, candidate := range admission.Accepted {
		names[i] = candidate.Name
	}
	if !slices.Equal(names, accepted) || len(admission.Headers) != len(names) || len(admission.Failures) != len(failures) {
		tb.Fatalf("admitted %v with %d headers and failures %+v, want %v and failures %+v", names, len(admission.Headers), admission.Failures, accepted, failures)
	}
	for i, failure := range failures {
		if got := admission.Failures[i]; got.Name != failure.Name || !errors.Is(got.Err, failure.Err) {
			tb.Fatalf("failure %d = %+v, want %+v", i, got, failure)
		}
	}
}

func TestAdmitImagesEnforcesTheCountLimit(t *testing.T) {
	var candidates []ImageCandidate
	var names []string
	for i := range ImageCountLimit + 1 {
		names = append(names, fmt.Sprintf("image-%d.png", i))
		candidates = append(candidates, ImageCandidate{Name: names[i], Data: imageSizedPNG(t, 1000)})
	}
	imageRequireAdmission(t, AdmitImages(nil, candidates), names[:ImageCountLimit], ImageFailure{Name: "image-10.png", Err: ErrImageLimit})

	existing := []int64{1, 1, 1, 1, 1, 1, 1, 1, 1}
	before := slices.Clone(existing)
	imageRequireAdmission(t, AdmitImages(existing, candidates[:2]), names[:1], ImageFailure{Name: "image-1.png", Err: ErrImageLimit})
	if !slices.Equal(existing, before) {
		t.Fatalf("existing sizes changed to %v", existing)
	}
}

func TestAdmitImagesEnforcesTheAggregateLimit(t *testing.T) {
	admission := AdmitImages([]int64{4 << 20}, []ImageCandidate{
		{Name: "a.png", Data: imageSizedPNG(t, 4<<20)},
		{Name: "b.png", Data: imageSizedPNG(t, 3<<20)},
		{Name: "c.png", Data: imageSizedPNG(t, 2<<20)},
	})
	imageRequireAdmission(t, admission, []string{"a.png", "c.png"}, ImageFailure{Name: "b.png", Err: ErrImageLimit})

	saturated := AdmitImages([]int64{math.MaxInt64, math.MaxInt64}, []ImageCandidate{{Name: "d.png", Data: imageSizedPNG(t, 100)}})
	imageRequireAdmission(t, saturated, nil, ImageFailure{Name: "d.png", Err: ErrImageLimit})
}

func TestAdmitImagesEnforcesTheEncodedLimitPerImage(t *testing.T) {
	admission := AdmitImages(nil, []ImageCandidate{{Name: "e.png", Data: imageSizedPNG(t, ImageEncodedLimit+1)}, {Name: "f.png", Data: imageSizedPNG(t, ImageEncodedLimit)}})
	imageRequireAdmission(t, admission, []string{"f.png"}, ImageFailure{Name: "e.png", Err: ErrImageLimit})
}

func TestAdmitImagesReportsFailuresWithoutDroppingAccepted(t *testing.T) {
	good := imageEncodedPNG(t, imageOpaqueRGBA(3, 3))
	admission := AdmitImages([]int64{500}, []ImageCandidate{
		{Name: "first.png", Data: good},
		{Name: "broken.png", Data: good[:20]},
		{Name: "second.png", Data: good},
		{Name: "letter.pdf", Data: []byte("%PDF-1.7")},
	})
	imageRequireAdmission(t, admission, []string{"first.png", "second.png"},
		ImageFailure{Name: "broken.png", Err: ErrImageHeader},
		ImageFailure{Name: "letter.pdf", Err: ErrImageFormat},
	)
	if admission.Headers[1].Width != 3 || !bytes.Equal(admission.Accepted[0].Data, good) {
		t.Fatalf("accepted %+v with header %+v, want the original 3x3 bytes", admission.Accepted[0].Name, admission.Headers[1])
	}
}
