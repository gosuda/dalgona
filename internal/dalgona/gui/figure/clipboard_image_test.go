package figure

import (
	"bytes"
	"context"
	"encoding/binary"
	"image"
	"image/color"
	"image/png"
	"io"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

func TestClipboardPNGReplyChecksLengthsHeadersAndDigest(t *testing.T) {
	data := imageEncodedPNG(t, image.NewNRGBA(image.Rect(0, 0, 2, 1)))
	first, second := data[:11], data[11:]
	head := envelope{Type: messageResult, Job: 3, Key: "key", Kind: resultClipboardImage, Budget: len(data), PNGBytes: len(data), Chunks: 2, Bounds: []float64{2, 1}}
	c0 := frame{env: envelope{Type: messageChunk, Job: 3, Index: 0, Format: clipboardimage.PNG}, payload: first}
	c1 := frame{env: envelope{Type: messageChunk, Job: 3, Index: 1, Format: clipboardimage.PNG}, payload: second}
	end := frame{env: envelope{Type: messageComplete, Job: 3, Chunks: 2, Bytes: len(data), Digest: chunkDigest(first, second)}}
	offer := &recordedClipboard{}
	c := clipboardCollector(offer)
	if done, healthy := feed(c, frame{env: head}, c0, c1); done || !healthy || offer.published {
		t.Fatal("the encoded image became visible before its completion frame")
	}
	if done, healthy := c.accept(end); !done || !healthy || !bytes.Equal(offer.png.Bytes(), data) {
		t.Fatalf("verified clipboard image did not stage: %v", c.outcome().Err)
	}
	excess, wrongBounds, badEnd, badChunk := head, head, end, c1
	excess.PNGBytes = clipboardimage.MaxBytes + 1
	wrongBounds.Bounds = []float64{1, 2}
	badEnd.env.Digest = chunkDigest(second, first)
	badChunk.env.Format = clipboardimage.DIB
	checkRejectedClipboardReplies(t, map[string][]frame{
		"over budget":     {{env: excess}},
		"bounds mismatch": {{env: wrongBounds}, c0, c1, end},
		"digest mismatch": {{env: head}, c0, c1, badEnd},
		"out of order":    {{env: head}, c0, badChunk, end},
	})
}

func checkRejectedClipboardReplies(t *testing.T, invalid map[string][]frame) {
	t.Helper()
	for name, frames := range invalid {
		offer := &recordedClipboard{}
		c := clipboardCollector(offer)
		if done, healthy := feed(c, frames...); !done || healthy || offer.published {
			t.Errorf("%s: clipboard image was not rejected: %v", name, c.outcome().Err)
		}
	}
}

func clipboardCollector(offer *recordedClipboard) *collector {
	j := testJob(messageClipboardImage)
	j.request.Kind, j.clipboard = clipboardimage.PNG, offer
	return newCollector(j)
}

func TestClipboardRendererStreamsOriginalPNGAndSmallDIB(t *testing.T) {
	size := image.Pt(256, 144)
	stored := encodeClipboardFixtureImage(t, size)
	offer := &recordedClipboard{wantDIB: true}
	exportClipboardFixture(t, stored, offer)
	if !offer.published || !bytes.Equal(offer.png.Bytes(), stored) {
		t.Fatal("the verified original PNG was not published byte for byte")
	}
	if offer.dib.length != clipboardimage.DIBLength(size) {
		t.Fatalf("DIB length = %d, want %d", offer.dib.length, clipboardimage.DIBLength(size))
	}
	const wantPixelBytes = 256 * 144 * clipboardimage.DIBPixelBytes
	if binary.LittleEndian.Uint32(offer.dib.start[:]) != clipboardimage.DIBHeaderSize || binary.LittleEndian.Uint32(offer.dib.start[20:]) != wantPixelBytes {
		t.Fatalf("DIB header lacks the expected bitmap declaration: %x", offer.dib.start)
	}
}

func TestPrepareClipboardImageConvertsLargeDIBInProcess(t *testing.T) {
	size := image.Pt(4096, 2304)
	stored := encodeClipboardFixtureImage(t, size)
	kind := clipboardPNGDIB
	env := envelope{Type: messageClipboardImage, Kind: kind, Key: clipboardKey(kind, ImagePNG.String(), stored), Format: ImagePNG.String()}
	out, err := prepareClipboardImage(context.Background(), env, stored)
	if err != nil {
		t.Fatal(err)
	}
	dib := collectSentDIB(t, out, env)
	if len(dib) != 36<<20+108 {
		t.Fatalf("DIB length = %d, want 36 MiB plus its 108-byte header", len(dib))
	}
	const wantPixelBytes = 36 << 20
	if binary.LittleEndian.Uint32(dib) != 108 || binary.LittleEndian.Uint32(dib[20:]) != wantPixelBytes {
		t.Fatalf("DIB header lacks the expected full-size bitmap declaration: %x", dib[:108])
	}
	bottom := [4]byte(dib[108:112])
	topOffset := 108 + (size.Y-1)*size.X*clipboardimage.DIBPixelBytes
	top := [4]byte(dib[topOffset : topOffset+4])
	if bottom != ([4]byte{0x78, 0x67, 0x56, 0x9a}) || top != ([4]byte{0x34, 0x23, 0x12, 0x80}) {
		t.Fatalf("DIB has wrong bottom/top BGRA: %x / %x", bottom, top)
	}
}

func collectSentDIB(t *testing.T, out *clipboardResult, env envelope) []byte {
	t.Helper()
	var buf bytes.Buffer
	if err := out.send(context.Background(), &buf, env); err != nil {
		t.Fatal(err)
	}
	var dib []byte
	for {
		fenv, payload, err := readFrame(&buf, ReplyFrameLimit)
		if err != nil {
			t.Fatal(err)
		}
		if fenv.Type == messageComplete {
			return dib
		}
		if fenv.Type == messageChunk && fenv.Format == clipboardimage.DIB {
			dib = append(dib, payload...)
		}
	}
}

func encodeClipboardFixtureImage(t *testing.T, size image.Point) []byte {
	t.Helper()
	img := image.NewNRGBA(image.Rectangle{Max: size})
	img.SetNRGBA(0, 0, color.NRGBA{R: 0x12, G: 0x23, B: 0x34, A: 0x80})
	img.SetNRGBA(0, size.Y-1, color.NRGBA{R: 0x56, G: 0x67, B: 0x78, A: 0x9a})
	var stored bytes.Buffer
	encoder := png.Encoder{CompressionLevel: png.BestSpeed}
	if err := encoder.Encode(&stored, img); err != nil {
		t.Fatal(err)
	}
	return stored.Bytes()
}

func exportClipboardFixture(t *testing.T, source []byte, offer *recordedClipboard) {
	t.Helper()
	s, err := NewSupervisor(SupervisorConfig{Fonts: testFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(s.Close)
	id, err := s.ExportClipboardImage(ImagePNG, source, offer, nil)
	if err != nil {
		t.Fatal(err)
	}
	waitClipboardOutcome(t, s, id)
	got, err, ok := s.TakeClipboardImage(id)
	if err != nil || !ok || got != offer {
		t.Fatalf("the verified offer did not return: %v, ready %t", err, ok)
	}
	defer got.Close()
	if err := got.Publish(); err != nil {
		t.Fatal(err)
	}
}

func waitClipboardOutcome(t *testing.T, s *Supervisor, id uint64) {
	t.Helper()
	select {
	case o := <-s.Outcomes():
		if o.Job != id || o.Err != nil {
			t.Fatalf("renderer rejected the clipboard image: %v", o.Err)
		}
	case <-time.After(30 * time.Second):
		t.Fatal("the renderer did not return a clipboard result")
	}
}

type recordedClipboard struct {
	wantDIB   bool
	published bool
	png       bytes.Buffer
	dib       clipboardDIBSample
}

func (r *recordedClipboard) NeedsDIB() bool { return r.wantDIB }

func (r *recordedClipboard) Stage(s clipboardimage.Spec) (io.Writer, io.Writer, error) {
	r.dib.topOffset = 108 + (s.Size.Y-1)*s.Size.X*4
	return &r.png, &r.dib, nil
}

func (r *recordedClipboard) Publish() error {
	r.published = true
	return nil
}

func (*recordedClipboard) Close() {}

type clipboardDIBSample struct {
	start     [112]byte
	top       [4]byte
	topOffset int
	length    int
}

func (s *clipboardDIBSample) Write(p []byte) (int, error) {
	if s.length < len(s.start) {
		copy(s.start[s.length:], p)
	}
	from, to := max(s.length, s.topOffset), min(s.length+len(p), s.topOffset+len(s.top))
	if from < to {
		copy(s.top[from-s.topOffset:], p[from-s.length:to-s.length])
	}
	s.length += len(p)
	return len(p), nil
}
