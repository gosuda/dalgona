//go:build gui

package composer

import (
	"image"
	"os"
	"testing"
	"time"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestMain(m *testing.M) {
	if figure.IsChild(os.Args) {
		os.Exit(figure.ServeChild(figure.Fonts{}))
	}
	os.Exit(m.Run())
}

func pastePreviewPNG(t *testing.T) (*harness, *figure.Supervisor, int) {
	t.Helper()
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	ready := make(chan struct{}, 1)
	hub := figure.NewHub(sup, func() { ready <- struct{}{} })
	h := idleHarness(t, Host{Figures: hub}).withClipboard(platform.ClipboardContent{Image: pngBytes(t, 64, 32)})
	t.Cleanup(h.dock.Close)
	h.view.images[testThreadKey] = true
	before := sup.Pixels().Used()
	h.press("V", key.ModShortcut)
	h.awaitWake()
	select {
	case <-ready:
	case <-time.After(30 * time.Second):
		t.Fatal("the renderer child returned no image outcome")
	}
	h.frame()
	return h, sup, before
}

func TestPastedImageChipShowsRendererThumbnail(t *testing.T) {
	h, sup, before := pastePreviewPNG(t)
	node, ok := h.node("Thumbnail of Pasted image")
	if !ok || node.Desc.Bounds.Size() != image.Pt(32, 16) {
		t.Fatalf("image chip thumbnail bounds = %v, present = %v; want 32×16", node.Desc.Bounds, ok)
	}
	if sup.Launches() != 1 || sup.Pixels().Used()-before != 32*16*4 {
		t.Fatalf("renderer launches = %d, cached thumbnail bytes = %d", sup.Launches(), sup.Pixels().Used()-before)
	}
}

func TestRemovingImageChipReleasesSharedPixels(t *testing.T) {
	h, sup, before := pastePreviewPNG(t)
	if sup.Pixels().Used() <= before {
		t.Fatal("the image chip admitted no pixels before removal")
	}
	h.click("Remove")
	if got := sup.Pixels().Used(); got != before {
		t.Fatalf("cache usage after removing the chip = %d bytes; before adding it = %d bytes", got, before)
	}
}

func TestAttachingImageKeepsRestoredChipIdentityDistinct(t *testing.T) {
	h := newHarness(t, Host{})
	data := pngBytes(t, 4, 2)
	saved, err := imageChip("image-1", imageFile{name: "saved.png", data: data})
	if err != nil {
		t.Fatal(err)
	}
	h.view.drafts[testKey] = state.Draft{Attachments: []state.Attachment{saved}}
	h.frame()
	if err := h.dock.attachImage(h.env, imageFile{name: "new.png", data: data}); err != nil {
		t.Fatal(err)
	}
	chips := lastPut(h).Attachments
	if len(chips) != 2 || chips[0].ID != saved.ID || chips[1].ID == saved.ID {
		t.Fatalf("restored and new image chips must have distinct identities: %+v", chips)
	}
}
