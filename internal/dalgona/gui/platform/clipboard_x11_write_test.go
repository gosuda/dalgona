//go:build linux || freebsd || openbsd

package platform

import (
	"bytes"
	"crypto/rand"
	"errors"
	"image"
	"image/png"
	"io"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

func TestX11ClipboardWritesImageForImagePaste(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	png := testPNG(t, 3, 2)
	for range 2 {
		publishTestPNG(t, clipboard.ImageClipboard(t.Context()), image.Pt(3, 2), png)
	}
	got, err := readX11Clipboard(t, clipboard, PasteImage)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got.Image, png) || got.ImageType != imagePNGMIME {
		t.Fatalf("pasted %d bytes of %q, want the %d offered PNG bytes", len(got.Image), got.ImageType, len(png))
	}
}

func noisyTestPNG(t *testing.T, width, height int) []byte {
	t.Helper()
	img := image.NewNRGBA(image.Rect(0, 0, width, height))
	if _, err := rand.Read(img.Pix); err != nil {
		t.Fatal(err)
	}
	var encoded bytes.Buffer
	if err := png.Encode(&encoded, img); err != nil {
		t.Fatal(err)
	}
	return encoded.Bytes()
}

func TestX11ClipboardWritesLargeImageExactly(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	png := noisyTestPNG(t, 1024, 1024)
	if len(png) <= 4*clipboard.selection.maxRequestUnits {
		t.Skipf("the %d-byte PNG fits one request on this server; start a server with a smaller maximum request", len(png))
	}
	publishTestPNG(t, clipboard.ImageClipboard(t.Context()), image.Pt(1024, 1024), png)
	got, err := readX11Clipboard(t, clipboard, PasteImage)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got.Image, png) || got.ImageType != imagePNGMIME {
		t.Fatalf("pasted %d bytes of %q, want the %d offered PNG bytes", len(got.Image), got.ImageType, len(png))
	}
}

func TestX11IncompleteClipboardOfferKeepsPreviousImage(t *testing.T) {
	display := startX11Server(t, withoutTCP)
	clipboard := openX11ClipboardForTest(t, display)
	previous := testPNG(t, 3, 2)
	publishTestPNG(t, clipboard.ImageClipboard(t.Context()), image.Pt(3, 2), previous)
	offer := clipboard.ImageClipboard(t.Context())
	defer offer.Close()
	if _, _, err := offer.Stage(clipboardimage.Spec{Size: image.Pt(3, 2), PNGBytes: len(previous)}); err != nil {
		t.Fatal(err)
	}
	if err := offer.Publish(); !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("publishing an incomplete image: %v", err)
	}
	got, err := readX11Clipboard(t, clipboard, PasteImage)
	if err != nil || !bytes.Equal(got.Image, previous) {
		t.Fatalf("incomplete offer replaced the previous image: %v", err)
	}
}
