//go:build gui

package transcript

import (
	"bytes"
	"context"
	"image"
	"image/color"
	"image/gif"
	"image/jpeg"
	"image/png"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestRecordedNonPNGImageCopyPreservesPixelsAndSavePreservesBytes(t *testing.T) {
	for _, tc := range []struct {
		name, mime string
		fixture    func(*testing.T) ([]byte, image.Image)
	}{
		{name: "map.jpg", mime: "image/jpeg", fixture: clipboardJPEG},
		{name: "map.gif", mime: "image/gif", fixture: clipboardGIF},
	} {
		t.Run(tc.name, func(t *testing.T) {
			stored, pixels := tc.fixture(t)
			path := filepath.Join(t.TempDir(), tc.name)
			var copied []byte
			deps, woke := imageClipboardDeps(t, func(data []byte) error {
				copied = bytes.Clone(data)
				return nil
			})
			deps.Save = func(context.Context, platform.DialogRequest) (platform.DialogResult, error) {
				return platform.DialogResult{Files: []string{path}}, nil
			}
			withTranscriptLive(t, deps, storedImageTurn(tc.name, tc.mime, stored), func(t *testing.T, h *harness) {
				waitForImageRenderer(t, woke)
				h.frame()
				h.click("Copy image 1")
				waitFor(t, "the converted clipboard image", func() bool {
					h.frame()
					return len(copied) > 0 || strings.HasPrefix(h.tr.notice, "Copy image failed:")
				})
				if h.tr.notice != "Copied image 1." {
					t.Fatalf("copy notice = %q", h.tr.notice)
				}
				got, err := png.Decode(bytes.NewReader(copied))
				if err != nil {
					t.Fatalf("clipboard did not receive PNG: %v", err)
				}
				checkClipboardPixels(t, got, pixels)
				checkSavedImage(t, h, path, stored)
			})
		})
	}
}

func checkSavedImage(t *testing.T, h *harness, path string, stored []byte) {
	t.Helper()
	h.click("Save image 1")
	waitFor(t, "the original image export", func() bool {
		_, err := os.Stat(path)
		return err == nil
	})
	saved, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(saved, stored) {
		t.Fatalf("saved %d bytes, %v; want the exact %d encoded input bytes", len(saved), err, len(stored))
	}
}

func TestRecordedImageCopyDiscardsCompletedConversionAfterThreadChange(t *testing.T) {
	stored, _ := clipboardGIF(t)
	writes := 0
	deps, woke := imageClipboardDeps(t, func(_ []byte) error {
		writes++
		return nil
	})
	withTranscriptLive(t, deps, storedImageTurn("map.gif", "image/gif", stored), func(t *testing.T, h *harness) {
		waitForImageRenderer(t, woke)
		h.frame()
		original := h.key
		r := h.tr.panes[original].rows[identity(original, "u1", "i1")]
		h.tr.startImageCopy(r, r.sheet.images[0], r.images[0])
		if h.tr.notice != preparingImageCopy {
			t.Fatalf("copy did not start: %q", h.tr.notice)
		}
		waitForImageRenderer(t, woke)
		h.key.Thread = "closed-thread"
		h.frame()
		h.key = original
		h.frame()
		if writes != 0 {
			t.Fatal("a completed clipboard conversion wrote after its thread was left")
		}
		h.click("Copy image 1")
		waitFor(t, "a fresh clipboard write", func() bool {
			h.frame()
			return writes != 0 || strings.HasPrefix(h.tr.notice, "Copy image failed:")
		})
		if writes != 1 {
			t.Fatalf("fresh copy wrote %d times: %q", writes, h.tr.notice)
		}
	})
}

func TestRecordedImageCopyPublishesRendererFailure(t *testing.T) {
	stored, _ := clipboardJPEG(t)
	scan := bytes.Index(stored, []byte{0xff, 0xda})
	if scan < 0 {
		t.Fatal("fixture JPEG has no start-of-scan marker")
	}
	pngData := pngOf(t)
	idat := bytes.Index(pngData, []byte("IDAT"))
	if idat < 0 {
		t.Fatal("fixture PNG has no image data marker")
	}
	t.Run("JPEG", func(t *testing.T) {
		checkDamagedStoredImageCopy(t, "damaged.jpg", "image/jpeg", stored[:scan])
	})
	t.Run("PNG", func(t *testing.T) {
		checkDamagedStoredImageCopy(t, "damaged.png", "image/png", pngData[:idat+4])
	})
}

func checkDamagedStoredImageCopy(t *testing.T, name, mime string, damaged []byte) {
	t.Helper()
	writes := 0
	deps, woke := imageClipboardDeps(t, func(_ []byte) error {
		writes++
		return nil
	})
	withTranscriptLive(t, deps, storedImageTurn(name, mime, damaged), func(t *testing.T, h *harness) {
		waitForImageRenderer(t, woke)
		h.frame()
		for attempt := range 2 {
			h.click("Copy image 1")
			waitFor(t, "the image copy failure", func() bool {
				h.frame()
				return strings.HasPrefix(h.tr.notice, "Copy image failed:")
			})
			if writes != 0 || !strings.Contains(h.tr.notice, "damaged") {
				t.Fatalf("attempt %d wrote %d times: %q", attempt+1, writes, h.tr.notice)
			}
		}
	})
}

func storedImageTurn(name, mime string, data []byte) func(*daemon) []rpc.HistoryTurn {
	return func(d *daemon) []rpc.HistoryTurn {
		d.images["i1#0"] = data
		meta := map[string]any{"type": "image", "name": name, "url": "dalgona-blob:x", "mime": mime, "bytes": len(data)}
		return []rpc.HistoryTurn{frozenTurn("u1", rpc.StatusCompleted,
			d.header("i1", rpc.KindUser, map[string]any{"content": []any{meta}}, fieldData{name: "text", data: "Reference image"}),
		)}
	}
}

func imageClipboardDeps(t *testing.T, write func([]byte) error) (Deps, <-chan struct{}) {
	t.Helper()
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	woke := make(chan struct{}, 1)
	hub := figure.NewHub(sup, func() {
		select {
		case woke <- struct{}{}:
		default:
		}
	})
	return Deps{Figures: hub, CopyImage: func() (clipboardimage.Offer, error) {
		return platform.NewPNGClipboardOffer(t.Context(), func(_ context.Context, png []byte) error {
			return write(png)
		}), nil
	}}, woke
}

func waitForImageRenderer(t *testing.T, woke <-chan struct{}) {
	t.Helper()
	select {
	case <-woke:
	case <-time.After(10 * time.Second):
		t.Fatal("the renderer child did not publish an image outcome")
	}
}

func clipboardJPEG(t *testing.T) ([]byte, image.Image) {
	t.Helper()
	gray := image.NewGray(image.Rect(0, 0, 17, 9))
	for y := range gray.Rect.Dy() {
		for x := range gray.Rect.Dx() {
			gray.SetGray(x, y, color.Gray{Y: uint8(20 + 9*x + 5*y)})
		}
	}
	var data bytes.Buffer
	if err := jpeg.Encode(&data, gray, nil); err != nil {
		t.Fatal(err)
	}
	want, err := jpeg.Decode(bytes.NewReader(data.Bytes()))
	if err != nil {
		t.Fatal(err)
	}
	return data.Bytes(), want
}

func clipboardGIF(t *testing.T) ([]byte, image.Image) {
	t.Helper()
	bounds := image.Rect(0, 0, 3, 2)
	palette := color.Palette{color.Transparent, color.RGBA{R: 200, A: 255}, color.RGBA{G: 200, A: 255}}
	first := image.NewPaletted(bounds, palette)
	first.SetColorIndex(2, 1, 1)
	second := image.NewPaletted(bounds, palette)
	second.SetColorIndex(0, 0, 2)
	var data bytes.Buffer
	if err := gif.EncodeAll(&data, &gif.GIF{Image: []*image.Paletted{first, second}, Delay: []int{1, 1}}); err != nil {
		t.Fatal(err)
	}
	return data.Bytes(), first
}

func checkClipboardPixels(t *testing.T, got, want image.Image) {
	t.Helper()
	b := want.Bounds()
	if got.Bounds() != b {
		t.Fatalf("clipboard PNG bounds = %v, want %v", got.Bounds(), b)
	}
	for y := b.Min.Y; y < b.Max.Y; y++ {
		for x := b.Min.X; x < b.Max.X; x++ {
			g := color.NRGBAModel.Convert(got.At(x, y))
			w := color.NRGBAModel.Convert(want.At(x, y))
			if g != w {
				t.Fatalf("clipboard pixel (%d,%d) = %v, want %v", x, y, g, w)
			}
		}
	}
}
