package figure

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"hash"
	"image"
	"image/png"
	"io"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

const clipboardPNGDIB = "png+dib"

func clipboardKey(kind, format string, data []byte) string {
	return digestOf([]byte(messageClipboardImage), []byte(kind), []byte(format), data)
}

func (c *child) clipboardImage(ctx context.Context, env envelope, data []byte) error {
	out, err := prepareClipboardImage(ctx, env, data)
	if err == nil {
		err = out.send(ctx, c.out, env)
	}
	if err != nil {
		return writeFrame(c.out, failureEnvelope(env.Job, err), nil, ReplyFrameLimit)
	}
	return nil
}

type clipboardResult struct {
	spec clipboardimage.Spec
	png  []resultChunk
	dib  image.Image
}

func prepareClipboardImage(ctx context.Context, env envelope, data []byte) (*clipboardResult, error) {
	if env.Kind != clipboardimage.PNG && env.Kind != clipboardPNGDIB || clipboardKey(env.Kind, env.Format, data) != env.Key {
		return nil, fail(FailureProtocol, "clipboard image request has an invalid format or source key")
	}
	if env.Format == ImagePNG.String() {
		return originalClipboardPNG(env.Kind, data)
	}
	img, err := decodeJobImage(env.Format, data)
	if err != nil {
		return nil, err
	}
	out := &clipboardResult{spec: clipboardimage.Spec{Size: img.Bounds().Size()}}
	if env.Kind == clipboardPNGDIB {
		out.spec.DIBBytes = clipboardimage.DIBLength(out.spec.Size)
		out.dib = img
	}
	if err := out.encodePNG(ctx, img); err != nil {
		return nil, err
	}
	return out, nil
}

func (r *clipboardResult) encodePNG(ctx context.Context, img *image.NRGBA) error {
	limit := clipboardimage.MaxBytes - r.spec.DIBBytes
	out := clipboardPNGWriter{ctx: ctx, left: limit}
	encoder := png.Encoder{CompressionLevel: png.BestSpeed}
	if err := encoder.Encode(&out, img); err != nil {
		return err
	}
	r.png = out.chunks
	r.spec.PNGBytes = limit - out.left
	return nil
}

func originalClipboardPNG(kind string, data []byte) (*clipboardResult, error) {
	header, err := checkJobImage(ImagePNG.String(), data)
	if err != nil {
		return nil, err
	}
	img, err := decodeCheckedFrame(data, header.Format)
	if err != nil {
		return nil, imageFailure(err)
	}
	out := &clipboardResult{spec: clipboardimage.Spec{Size: img.Bounds().Size(), PNGBytes: len(data)}, png: splitPayload(data)}
	if kind == clipboardPNGDIB {
		out.spec.DIBBytes = clipboardimage.DIBLength(out.spec.Size)
		out.dib = img
	}
	return out, nil
}

type clipboardPNGWriter struct {
	ctx    context.Context
	chunks []resultChunk
	left   int
}

func (p *clipboardPNGWriter) Write(data []byte) (int, error) {
	if p.ctx.Err() != nil {
		return 0, fail(FailureTimeout, "clipboard image encoding stopped at its deadline")
	}
	n := len(data)
	if n > p.left {
		return 0, fail(FailureLimit, "clipboard image output exceeds %d bytes", clipboardimage.MaxBytes)
	}
	p.left -= n
	for len(data) != 0 {
		i := len(p.chunks) - 1
		if i < 0 || len(p.chunks[i].payload) == chunkPayloadLimit {
			p.chunks = append(p.chunks, resultChunk{})
			i++
		}
		chunk := &p.chunks[i].payload
		size := min(len(data), chunkPayloadLimit-len(*chunk))
		*chunk = append(*chunk, data[:size]...)
		data = data[size:]
	}
	return n, nil
}

type clipboardFrames struct {
	ctx    context.Context
	out    io.Writer
	digest hash.Hash
	job    uint64
	next   int
}

func (r *clipboardResult) send(ctx context.Context, w io.Writer, req envelope) error {
	if err := r.spec.Check(); err != nil {
		return fail(FailureLimit, "%v", err)
	}
	width, height := r.spec.Size.X, r.spec.Size.Y
	stride := width * clipboardimage.DIBPixelBytes
	rowsPerChunk := chunkPayloadLimit / stride
	count := len(r.png)
	if r.dib != nil {
		count += 1 + (height+rowsPerChunk-1)/rowsPerChunk
	}
	budget := r.spec.PNGBytes + r.spec.DIBBytes
	head := envelope{
		Type: messageResult, Job: req.Job, Key: req.Key, Kind: resultClipboardImage, Chunks: count,
		Budget: budget, Bounds: []float64{float64(width), float64(height)}, PNGBytes: r.spec.PNGBytes, DIBBytes: r.spec.DIBBytes,
	}
	if err := writeFrame(w, &head, nil, ReplyFrameLimit); err != nil {
		return err
	}
	out := clipboardFrames{ctx: ctx, out: w, digest: sha256.New(), job: req.Job}
	for _, chunk := range r.png {
		if err := out.chunk(clipboardimage.PNG, chunk.payload); err != nil {
			return err
		}
	}
	if r.dib != nil {
		if err := out.bitmap(r.dib, rowsPerChunk); err != nil {
			return err
		}
	}
	return writeFrame(w, &envelope{Type: messageComplete, Job: req.Job, Chunks: out.next, Bytes: budget, Digest: hex.EncodeToString(out.digest.Sum(nil))}, nil, ReplyFrameLimit)
}

func (f *clipboardFrames) chunk(format string, data []byte) error {
	if f.ctx.Err() != nil {
		return fail(FailureTimeout, "clipboard image export stopped at its deadline")
	}
	frame := envelope{Type: messageChunk, Job: f.job, Index: f.next, Format: format}
	f.next++
	f.digest.Write(data)
	return writeFrame(f.out, &frame, data, ReplyFrameLimit)
}

func (f *clipboardFrames) bitmap(img image.Image, rowsPerChunk int) error {
	bounds := img.Bounds()
	size := bounds.Size()
	header := clipboardimage.DIBHeader(size)
	if err := f.chunk(clipboardimage.DIB, header[:]); err != nil {
		return err
	}
	stride := size.X * clipboardimage.DIBPixelBytes
	buffer := make([]byte, stride*min(rowsPerChunk, size.Y))
	rowInto := bitmapRowWriter(img)
	for top := size.Y; top > 0; top -= rowsPerChunk {
		rows := min(top, rowsPerChunk)
		for row := range rows {
			rowInto(buffer[row*stride:], bounds.Min.Y+top-row-1)
		}
		if err := f.chunk(clipboardimage.DIB, buffer[:rows*stride]); err != nil {
			return err
		}
	}
	return nil
}

func bitmapRowWriter(img image.Image) func([]byte, int) {
	bounds := img.Bounds()
	if rgba, ok := img.(*image.NRGBA); ok {
		stride := bounds.Dx() * clipboardimage.DIBPixelBytes
		return func(dst []byte, y int) {
			offset := rgba.PixOffset(bounds.Min.X, y)
			bgraRow(dst, rgba.Pix[offset:offset+stride])
		}
	}
	source := imageSource(img)
	return func(dst []byte, y int) {
		for x := range bounds.Dx() {
			pixel := source(bounds.Min.X+x, y)
			d := dst[x*4 : x*4+4]
			d[0], d[1], d[2], d[3] = pixel.B, pixel.G, pixel.R, pixel.A
		}
	}
}

func bgraRow(dst, src []byte) {
	for i := 0; i < len(src); i += clipboardimage.DIBPixelBytes {
		pixel := binary.LittleEndian.Uint32(src[i:])
		binary.LittleEndian.PutUint32(dst[i:], pixel&0xff00ff00|(pixel&0xff)<<16|pixel>>16&0xff)
	}
}
