package platform

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"image"
	"image/png"
	"math/bits"
)

const (
	dibInfoHeaderSize       = 40
	dibV4HeaderSize         = 108
	dibV5HeaderSize         = 124
	dibAlphaMaskOffset      = 52
	dibCompressionRGB       = 0
	dibCompressionBitfields = 3
	dibMaskBytes            = 4
	dibColorEntryBytes      = 4
	dibRowAlignmentBits     = 32
)

var (
	errDIBTruncated   = errors.New("the copied bitmap ends before its header, color masks, color table, or pixels end; copy the image again")
	errDIBUnsupported = errors.New("the copied bitmap uses a header, bit depth, or compression that dalgona cannot read; copy the image as a 24-bit or 32-bit bitmap or as PNG")
	errDIBInvalid     = errors.New("the copied bitmap declares invalid dimensions, planes, or color masks; copy the image again")
)

type dibInfoHeader struct {
	Size        uint32
	Width       int32
	Height      int32
	Planes      uint16
	BitCount    uint16
	Compression uint32
	_           [12]byte
	ColorsUsed  uint32
	_           uint32
}

type dibChannel struct {
	mask  uint32
	shift int
	scale uint64
}

type dibLayout struct {
	width         int
	rows          int
	stride        int
	pixels        int
	pixelBytes    int
	topDown       bool
	alphaExplicit bool
	red           dibChannel
	green         dibChannel
	blue          dibChannel
	alpha         dibChannel
}

func pngFromDIB(dib []byte) ([]byte, error) {
	layout, err := parseDIB(dib)
	if err != nil {
		return nil, err
	}
	var encoded bytes.Buffer
	if err := png.Encode(&encoded, layout.decode(dib[layout.pixels:])); err != nil {
		return nil, fmt.Errorf("encode the copied bitmap as PNG: %w; copy the image again", err)
	}
	return encoded.Bytes(), nil
}

func parseDIB(dib []byte) (dibLayout, error) {
	header, err := readDIBHeader(dib)
	if err != nil {
		return dibLayout{}, err
	}
	layout, err := header.geometry()
	if err != nil {
		return dibLayout{}, err
	}
	masks, maskBytes, err := header.masks(dib)
	if err != nil {
		return dibLayout{}, err
	}
	if err := layout.setChannels(masks); err != nil {
		return dibLayout{}, err
	}
	layout.alphaExplicit = header.BitCount == 32 && (header.Compression == dibCompressionBitfields || header.Size != dibInfoHeaderSize)
	offset := int64(header.Size) + maskBytes + dibColorEntryBytes*int64(header.ColorsUsed)
	if offset+int64(layout.stride)*int64(layout.rows) > int64(len(dib)) {
		return dibLayout{}, errDIBTruncated
	}
	layout.pixels = int(offset)
	return layout, nil
}

func readDIBHeader(dib []byte) (dibInfoHeader, error) {
	var header dibInfoHeader
	if len(dib) < 4 {
		return header, errDIBTruncated
	}
	switch size := binary.LittleEndian.Uint32(dib); size {
	case dibInfoHeaderSize, dibV4HeaderSize, dibV5HeaderSize:
		if len(dib) < int(size) {
			return header, errDIBTruncated
		}
	default:
		return header, fmt.Errorf("%w: the header holds %d bytes", errDIBUnsupported, size)
	}
	if _, err := binary.Decode(dib, binary.LittleEndian, &header); err != nil {
		return header, errDIBTruncated
	}
	return header, nil
}

func (h dibInfoHeader) geometry() (dibLayout, error) {
	switch {
	case h.Planes != 1 || h.Width <= 0 || h.Height == 0:
		return dibLayout{}, fmt.Errorf("%w: width %d, height %d, planes %d", errDIBInvalid, h.Width, h.Height, h.Planes)
	case h.BitCount != 24 && h.BitCount != 32:
		return dibLayout{}, fmt.Errorf("%w: %d bits per pixel", errDIBUnsupported, h.BitCount)
	}
	width, rows := int64(h.Width), int64(h.Height)
	topDown := rows < 0
	if topDown {
		rows = -rows
	}
	stride := (width*int64(h.BitCount) + dibRowAlignmentBits - 1) / dibRowAlignmentBits * 4
	if stride > transferLimit || rows > transferLimit/stride {
		return dibLayout{}, ErrTransferTooLarge
	}
	return dibLayout{
		width:      int(width),
		rows:       int(rows),
		stride:     int(stride),
		pixelBytes: int(h.BitCount / 8),
		topDown:    topDown,
	}, nil
}

func (h dibInfoHeader) masks(dib []byte) ([4]uint32, int64, error) {
	switch h.Compression {
	case dibCompressionRGB:
		return h.rgbMasks(dib), 0, nil
	case dibCompressionBitfields:
		return h.fieldMasks(dib)
	}
	return [4]uint32{}, 0, fmt.Errorf("%w: compression %d", errDIBUnsupported, h.Compression)
}

func (h dibInfoHeader) rgbMasks(dib []byte) [4]uint32 {
	masks := [4]uint32{0x00FF0000, 0x0000FF00, 0x000000FF, 0}
	switch {
	case h.Size != dibInfoHeaderSize:
		masks[3] = binary.LittleEndian.Uint32(dib[dibAlphaMaskOffset:])
	case h.BitCount == 32:
		masks[3] = 0xFF000000
	}
	return masks
}

func (h dibInfoHeader) fieldMasks(dib []byte) ([4]uint32, int64, error) {
	if h.BitCount != 32 {
		return [4]uint32{}, 0, fmt.Errorf("%w: color masks need 32 bits per pixel, not %d", errDIBInvalid, h.BitCount)
	}
	count, trailing := 4, int64(0)
	if h.Size == dibInfoHeaderSize {
		count, trailing = 3, 3*dibMaskBytes
	}
	if len(dib) < dibInfoHeaderSize+count*dibMaskBytes {
		return [4]uint32{}, 0, errDIBTruncated
	}
	var masks [4]uint32
	for i := range count {
		masks[i] = binary.LittleEndian.Uint32(dib[dibInfoHeaderSize+i*dibMaskBytes:])
	}
	return masks, trailing, nil
}

func (l *dibLayout) setChannels(masks [4]uint32) error {
	var used uint32
	for _, mask := range masks {
		if mask&used != 0 || !newDIBChannel(mask).contiguous() {
			return fmt.Errorf("%w: color masks %#08x overlap or are not contiguous", errDIBInvalid, masks)
		}
		used |= mask
	}
	if masks[0] == 0 || masks[1] == 0 || masks[2] == 0 {
		return fmt.Errorf("%w: color masks %#08x leave a red, green, or blue mask empty", errDIBInvalid, masks)
	}
	l.red, l.green, l.blue, l.alpha = newDIBChannel(masks[0]), newDIBChannel(masks[1]), newDIBChannel(masks[2]), newDIBChannel(masks[3])
	return nil
}

func newDIBChannel(mask uint32) dibChannel {
	shift := bits.TrailingZeros32(mask)
	return dibChannel{mask: mask, shift: shift, scale: max(uint64(mask>>shift), 1)}
}

func (c dibChannel) contiguous() bool {
	return c.scale&(c.scale+1) == 0
}

func (c dibChannel) sample(pixel uint32) uint8 {
	level := uint64((pixel & c.mask) >> c.shift)
	if c.scale != 0xFF {
		level = (level*0xFF + c.scale/2) / c.scale
	}
	return uint8(level) //nolint:gosec // level never exceeds 0xFF once scaled to eight bits
}

func (l dibLayout) decode(pixels []byte) *image.NRGBA {
	img := image.NewNRGBA(image.Rect(0, 0, l.width, l.rows))
	var alpha uint8
	for y := range l.rows {
		row := l.rowStart(y)
		alpha |= l.decodeRow(pixels[row:row+l.stride], img.Pix[y*img.Stride:(y+1)*img.Stride])
	}
	if alpha == 0 && (l.alpha.mask == 0 || !l.alphaExplicit) {
		for i := 3; i < len(img.Pix); i += 4 {
			img.Pix[i] = 0xFF
		}
	}
	return img
}

func (l dibLayout) rowStart(y int) int {
	if l.topDown {
		return y * l.stride
	}
	return (l.rows - 1 - y) * l.stride
}

func (l dibLayout) decodeRow(source, target []byte) uint8 {
	var alpha uint8
	for x := range l.width {
		pixel := dibPixel(source[x*l.pixelBytes : (x+1)*l.pixelBytes])
		color := target[4*x : 4*x+4]
		color[0], color[1], color[2], color[3] = l.red.sample(pixel), l.green.sample(pixel), l.blue.sample(pixel), l.alpha.sample(pixel)
		alpha |= color[3]
	}
	return alpha
}

func dibPixel(octets []byte) uint32 {
	var pixel uint32
	for i, octet := range octets {
		pixel |= uint32(octet) << (8 * i)
	}
	return pixel
}
