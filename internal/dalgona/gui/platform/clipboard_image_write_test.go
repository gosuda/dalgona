//go:build linux || freebsd || openbsd

package platform

import (
	"image"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

func publishTestPNG(t *testing.T, offer clipboardimage.Offer, size image.Point, png []byte) {
	t.Helper()
	defer offer.Close()
	w, _, err := offer.Stage(clipboardimage.Spec{Size: size, PNGBytes: len(png)})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := w.Write(png); err != nil {
		t.Fatal(err)
	}
	if err := offer.Publish(); err != nil {
		t.Fatal(err)
	}
}
