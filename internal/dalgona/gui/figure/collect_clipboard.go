package figure

import (
	"bytes"
	"encoding/binary"
	"image"
	"io"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

type clipboardCollection struct {
	size     image.Point
	original []byte
	png, dib clipboardPart
}

type clipboardPart struct {
	writer  io.Writer
	length  int
	written int
	prefix  [clipboardimage.DIBHeaderSize]byte
}

func clipboardReplySpec(env envelope, kind string) (clipboardimage.Spec, error) {
	b := env.Bounds
	if len(b) != 2 || !wholeSide(b[0], clipboardimage.MaxSide) || !wholeSide(b[1], clipboardimage.MaxSide) {
		return clipboardimage.Spec{}, fail(FailureProtocol, "clipboard image dimensions are out of bounds")
	}
	spec := clipboardimage.Spec{Size: image.Pt(int(b[0]), int(b[1])), PNGBytes: env.PNGBytes, DIBBytes: env.DIBBytes}
	if err := spec.Check(); err != nil || spec.PNGBytes+spec.DIBBytes != env.Budget {
		return clipboardimage.Spec{}, fail(FailureProtocol, "clipboard image declares invalid PNG or DIB lengths")
	}
	if (kind == clipboardPNGDIB) != (spec.DIBBytes != 0) {
		return clipboardimage.Spec{}, fail(FailureProtocol, "clipboard result formats do not match the request")
	}
	return spec, nil
}

func (c *collector) stageClipboard(env envelope) (*clipboardCollection, error) {
	if c.job.clipboard == nil {
		return nil, fail(FailureProtocol, "clipboard result arrived without a request")
	}
	spec, err := clipboardReplySpec(env, c.job.request.Kind)
	if err != nil {
		return nil, err
	}
	var original []byte
	if c.job.request.Format == ImagePNG.String() {
		original = c.job.payload
		if len(original) != spec.PNGBytes {
			return nil, fail(FailureProtocol, "clipboard export changed the length of the stored PNG")
		}
	}
	png, dib, err := c.job.clipboard.Stage(spec)
	if err != nil {
		return nil, fail(FailureEngine, "stage clipboard image: %v", err)
	}
	if png == nil || spec.DIBBytes != 0 && dib == nil {
		return nil, fail(FailureEngine, "clipboard staging did not provide every requested format")
	}
	return &clipboardCollection{
		size: spec.Size, original: original,
		png: clipboardPart{writer: png, length: spec.PNGBytes}, dib: clipboardPart{writer: dib, length: spec.DIBBytes},
	}, nil
}

func (c *clipboardCollection) write(format string, data []byte) error {
	if c.png.written < c.png.length {
		return c.writePNG(format, data)
	}
	return c.dib.write(c.size, clipboardimage.DIB, format, data, clipboardimage.DIBHeaderSize)
}

func (c *clipboardCollection) writePNG(format string, data []byte) error {
	from, to := c.png.written, c.png.written+len(data)
	if c.original != nil && (to > len(c.original) || !bytes.Equal(data, c.original[from:to])) {
		return fail(FailureProtocol, "clipboard export changed the stored PNG bytes")
	}
	return c.png.write(c.size, clipboardimage.PNG, format, data, clipboardimage.PNGHeaderSize)
}

func (p *clipboardPart) write(size image.Point, expected, format string, data []byte, headerBytes int) error {
	if format != expected || len(data) > p.length-p.written {
		return fail(FailureProtocol, "clipboard image chunk %q crosses its declared format boundary", format)
	}
	if p.written < headerBytes {
		copy(p.prefix[p.written:headerBytes], data)
		if p.written+len(data) >= headerBytes && !checkClipboardHeader(size, format, p.prefix[:headerBytes]) {
			return fail(FailureProtocol, "clipboard image %q header does not match its declared dimensions or layout", format)
		}
	}
	n, err := p.writer.Write(data)
	if err != nil || n != len(data) {
		return fail(FailureEngine, "stage clipboard image %q: %v", format, coalesceWriteError(err))
	}
	p.written += n
	return nil
}

func coalesceWriteError(err error) error {
	if err == nil {
		return io.ErrShortWrite
	}
	return err
}

func checkClipboardHeader(size image.Point, format string, header []byte) bool {
	if format == clipboardimage.DIB {
		want := clipboardimage.DIBHeader(size)
		return bytes.Equal(header, want[:])
	}
	if !bytes.HasPrefix(header, pngSignature) {
		return false
	}
	fields, err := pngHeaderChunk(header)
	if err != nil {
		return false
	}
	if _, err := pngLayout(fields); err != nil {
		return false
	}
	w, h := binary.BigEndian.Uint32(fields), binary.BigEndian.Uint32(fields[4:])
	return w <= clipboardimage.MaxSide && h <= clipboardimage.MaxSide && int(w) == size.X && int(h) == size.Y
}
