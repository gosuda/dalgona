package platform

import (
	"bytes"
	"context"
	"errors"
	"image"
	"io"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

func TestClipboardPNGOfferRejectsIncompleteOrStalePublication(t *testing.T) {
	png := testPNG(t, 2, 2)
	published := []byte("previous clipboard")
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	o := NewPNGClipboardOffer(ctx, func(_ context.Context, data []byte) error {
		published = data
		return nil
	})
	defer o.Close()
	w, _, err := o.Stage(clipboardimage.Spec{Size: image.Pt(2, 2), PNGBytes: len(png)})
	if err != nil {
		t.Fatal(err)
	}
	if err := o.Publish(); !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("publishing incomplete image: %v", err)
	}
	if _, err := w.Write(png); err != nil {
		t.Fatal(err)
	}
	cancel()
	if err := o.Publish(); !errors.Is(err, context.Canceled) {
		t.Fatalf("publishing stale image: %v", err)
	}
	if !bytes.Equal(published, []byte("previous clipboard")) {
		t.Fatal("an incomplete or stale image replaced the clipboard")
	}
}
