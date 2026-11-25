package clipboardimage

import (
	"encoding/binary"
	"errors"
	"image"
	"io"
)

const (
	PNG = "image/png"
	DIB = "application/x-win-dib"

	MaxBytes      = 256 << 20
	MaxSide       = 16_384
	MaxPixels     = 32_000_000
	PNGHeaderSize = 33
	DIBHeaderSize = 108
	DIBPixelBytes = 4
)

var ErrBounds = errors.New("the clipboard image has invalid dimensions or encoded lengths")

type Spec struct {
	Size     image.Point
	PNGBytes int
	DIBBytes int
}

func (s Spec) Check() error {
	if !ValidSize(s.Size) || s.PNGBytes < PNGHeaderSize || s.DIBBytes != 0 && s.DIBBytes != DIBLength(s.Size) || s.PNGBytes > MaxBytes-s.DIBBytes {
		return ErrBounds
	}
	return nil
}

func ValidSize(size image.Point) bool {
	return size.X > 0 && size.X <= MaxSide && size.Y > 0 && size.Y <= MaxSide && size.X*size.Y <= MaxPixels
}

func DIBLength(size image.Point) int { return DIBHeaderSize + DIBPixelBytes*size.X*size.Y }

func DIBHeader(size image.Point) [DIBHeaderSize]byte {
	var header [DIBHeaderSize]byte
	b := binary.LittleEndian
	b.PutUint32(header[:], DIBHeaderSize)
	b.PutUint32(header[4:], uint32(size.X)) //nolint:gosec // The export spec checks both dimensions before it builds the header.
	b.PutUint32(header[8:], uint32(size.Y)) //nolint:gosec // The export spec checks both dimensions before it builds the header.
	b.PutUint16(header[12:], 1)
	b.PutUint16(header[14:], 32)
	b.PutUint32(header[16:], 3)
	b.PutUint32(header[20:], uint32(DIBLength(size)-DIBHeaderSize)) //nolint:gosec // The checked 32-million-pixel DIB has at most 128 million bytes of rows.
	b.PutUint32(header[24:], 2835)
	b.PutUint32(header[28:], 2835)
	b.PutUint32(header[40:], 0x00ff0000)
	b.PutUint32(header[44:], 0x0000ff00)
	b.PutUint32(header[48:], 0x000000ff)
	b.PutUint32(header[52:], 0xff000000)
	return header
}

type Offer interface {
	NeedsDIB() bool
	Stage(Spec) (png, dib io.Writer, err error)
	Publish() error
	Close()
}
