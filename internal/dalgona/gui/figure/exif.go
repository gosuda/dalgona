package figure

import (
	"bytes"
	"encoding/binary"
	"image"
	"image/color"
)

const (
	exifOrientationTag = 0x0112
	exifShortType      = 3
	exifEntryBytes     = 12
)

var (
	exifIdentifier   = []byte("Exif\x00\x00")
	tiffLittleEndian = []byte("II*\x00")
	tiffBigEndian    = []byte("MM\x00*")
)

func exifOrientation(segment []byte) (int, bool) {
	tiff, found := bytes.CutPrefix(segment, exifIdentifier)
	if !found {
		return 1, false
	}
	order, found := tiffByteOrder(tiff)
	if !found {
		return 1, true
	}
	offset := uint64(order.Uint32(tiff[4:]))
	if offset+2 > uint64(len(tiff)) {
		return 1, true
	}
	entries := tiff[offset+2:]
	count := min(int(order.Uint16(tiff[offset:])), len(entries)/exifEntryBytes)
	for i := range count {
		entry := entries[exifEntryBytes*i:]
		if order.Uint16(entry) == exifOrientationTag {
			return exifOrientationValue(order, entry), true
		}
	}
	return 1, true
}

func tiffByteOrder(tiff []byte) (binary.ByteOrder, bool) {
	switch {
	case len(tiff) < 8:
		return nil, false
	case bytes.HasPrefix(tiff, tiffLittleEndian):
		return binary.LittleEndian, true
	case bytes.HasPrefix(tiff, tiffBigEndian):
		return binary.BigEndian, true
	}
	return nil, false
}

func exifOrientationValue(order binary.ByteOrder, entry []byte) int {
	if order.Uint16(entry[2:]) != exifShortType || order.Uint32(entry[4:]) != 1 {
		return 1
	}
	value := int(order.Uint16(entry[8:]))
	if value < 1 || value > 8 {
		return 1
	}
	return value
}

type imageOrientation struct{ transpose, mirrorX, mirrorY bool }

var imageOrientations = [9]imageOrientation{
	1: {},
	2: {mirrorX: true},
	3: {mirrorX: true, mirrorY: true},
	4: {mirrorY: true},
	5: {transpose: true},
	6: {transpose: true, mirrorY: true},
	7: {transpose: true, mirrorX: true, mirrorY: true},
	8: {transpose: true, mirrorX: true},
}

func (o imageOrientation) source(x, y, width, height int) (int, int) {
	if o.transpose {
		x, y = y, x
	}
	if o.mirrorX {
		x = width - 1 - x
	}
	if o.mirrorY {
		y = height - 1 - y
	}
	return x, y
}

func orientPixels(at func(x, y int) color.NRGBA, width, height int, orientation imageOrientation) *image.NRGBA {
	displayWidth, displayHeight := width, height
	if orientation.transpose {
		displayWidth, displayHeight = height, width
	}
	oriented := image.NewNRGBA(image.Rect(0, 0, displayWidth, displayHeight))
	pix := oriented.Pix
	for y := range displayHeight {
		for x := range displayWidth {
			pixel := at(orientation.source(x, y, width, height))
			pix[0], pix[1], pix[2], pix[3] = pixel.R, pixel.G, pixel.B, pixel.A
			pix = pix[rgbaPixelBytes:]
		}
	}
	return oriented
}
