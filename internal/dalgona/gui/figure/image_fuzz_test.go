package figure

import (
	"encoding/binary"
	"errors"
	"image"
	"image/color"
	"image/gif"
	"testing"
)

func FuzzInspectImage(f *testing.F) {
	pngData := imageEncodedPNG(f, imagePattern(5, 4))
	jpegData := imageEncodedJPEG(f, imageOpaqueRGBA(9, 7))
	palette := color.Palette{color.Black, color.White}
	gifData := imageEncodedGIF(f, &gif.GIF{
		LoopCount: 1,
		Image:     []*image.Paletted{image.NewPaletted(image.Rect(0, 0, 3, 2), palette), image.NewPaletted(image.Rect(1, 0, 3, 2), palette)},
		Delay:     []int{3, 3},
	})
	seeds := [][]byte{
		pngData,
		jpegData,
		gifData,
		pngData[:40],
		jpegData[:30],
		gifData[:20],
		imageWithExif(jpegData, imageExifSegment(binary.BigEndian, 8, imageOrientationEntry(7))),
		imageWithExif(jpegData, imageExifSegment(binary.LittleEndian, 4000, imageOrientationEntry(6))),
		imagePNGFile(imagePNGHeader(20000, 10, 8, 6), imageGarbageIDAT),
		imagePNGFile(imagePNGHeader(4100, 4100, 16, 6), imageGarbageIDAT),
		imagePNGFile(imagePNGHeader(4, 4, 16, 0), imagePNGChunk("tRNS", []byte{0, 0}), imageGarbageIDAT),
		imageJPEGFile(imageJPEGSegment(0xC0, imageJPEGFrame(17, 9, [3]byte{1, 0x22, 0}, [3]byte{2, 0x11, 1}, [3]byte{3, 0x11, 1}))),
		imageGIFFile(4, 4, [4]uint16{0, 0, 4, 4}, [4]uint16{2, 2, 3, 1}),
		[]byte("GIF89a"),
	}
	for _, seed := range seeds {
		f.Add(seed)
	}
	f.Fuzz(func(t *testing.T, data []byte) {
		header, err := InspectImage(data)
		if err != nil {
			if !errors.Is(err, ErrImageFormat) && !errors.Is(err, ErrImageHeader) && !errors.Is(err, ErrImageLimit) {
				t.Fatalf("unclassified error %v", err)
			}
			return
		}
		imageRequireHeaderWithinLimits(t, header, len(data))
		if header.PixelBytes <= 1<<18 {
			imageRequireDecodeAgrees(t, data, header)
		}
	})
}

func imageRequireHeaderWithinLimits(t *testing.T, header ImageHeader, encoded int) {
	t.Helper()
	switch {
	case encoded > ImageEncodedLimit, header.Format < ImagePNG, header.Format > ImageGIF:
	case header.Width < 1, header.Height < 1, header.Width > ImageSideLimit, header.Height > ImageSideLimit:
	case int64(header.Width)*int64(header.Height) > ImagePixelLimit:
	case header.PixelBytes < 1, header.PixelBytes > ImagePixelByteLimit:
	case header.Frames < 1, header.Format != ImageGIF && header.Frames != 1:
	case header.Orientation < 1, header.Orientation > 8:
	default:
		return
	}
	t.Fatalf("accepted header %+v for %d encoded bytes breaks a limit", header, encoded)
}

func imageRequireDecodeAgrees(t *testing.T, data []byte, header ImageHeader) {
	t.Helper()
	decoded, decodedHeader, err := DecodeImage(data)
	if err != nil {
		imageRequireError(t, err, ErrImageDecode)
		return
	}
	width, height := header.Width, header.Height
	if header.Orientation >= 5 {
		width, height = height, width
	}
	if decodedHeader != header || !decoded.Rect.Eq(image.Rect(0, 0, width, height)) {
		t.Fatalf("decoded %v with header %+v, want %dx%d with header %+v", decoded.Rect, decodedHeader, width, height, header)
	}
}
