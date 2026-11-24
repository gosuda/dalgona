//go:build gui

package composer

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"testing"
	"time"

	"gioui.org/io/key"
	"gioui.org/layout"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type blockingClipboard struct{ released chan struct{} }

func (b blockingClipboard) ReadClipboard(ctx context.Context, _ platform.PasteKind) (platform.ClipboardContent, error) {
	<-ctx.Done()
	close(b.released)
	return platform.ClipboardContent{}, ctx.Err()
}

func TestPasteReadCancelledOnClose(t *testing.T) {
	h := idleHarness(t, Host{})
	reader := blockingClipboard{released: make(chan struct{})}
	h.env.Clipboard = func() platform.ClipboardReader { return reader }
	h.dock.attach.paste(layout.Context{}, h.dock, h.env, platform.PasteOrdinary)
	h.dock.Close()
	select {
	case <-reader.released:
	case <-time.After(5 * time.Second):
		t.Fatal("closing the composer did not cancel the blocked clipboard read")
	}
}

func waitGoroutines(t *testing.T, want int) {
	t.Helper()
	for range 200 {
		if runtime.NumGoroutine() <= want {
			return
		}
		time.Sleep(25 * time.Millisecond)
	}
	t.Fatalf("goroutines did not settle to %d, have %d", want, runtime.NumGoroutine())
}

func TestDropSendAbortedOnClose(t *testing.T) {
	h := idleHarness(t, Host{})
	for range arrivalQueue {
		h.dock.attach.arrivals <- arrival{}
	}
	base := runtime.NumGoroutine()
	path := filepath.Join(t.TempDir(), "note.txt")
	if err := os.WriteFile(path, []byte("hello"), 0o600); err != nil {
		t.Fatal(err)
	}
	h.dock.Drop([]string{path})
	for range 200 {
		if runtime.NumGoroutine() > base {
			break
		}
		time.Sleep(25 * time.Millisecond)
	}
	h.dock.Close()
	waitGoroutines(t, base)
	select {
	case h.dock.attach.arrivals <- arrival{}:
		t.Fatal("the drop still occupied the arrival queue after close")
	default:
	}
	for range arrivalQueue {
		<-h.dock.attach.arrivals
	}
}

func writeSized(t *testing.T, dir, name string, size int) string {
	t.Helper()
	path := filepath.Join(dir, name)
	if err := os.WriteFile(path, make([]byte, size), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestReadImagesStopsAtAttachmentLimits(t *testing.T) {
	dir := t.TempDir()
	var paths []string
	for i := range imageLimit + 3 {
		paths = append(paths, writeSized(t, dir, "img-"+strconv.Itoa(i)+".bin", 64))
	}
	images, failed := readImages(paths)
	if len(images) != imageLimit || len(failed) != 3 {
		t.Fatalf("read %d images with %d failures, want %d and 3", len(images), len(failed), imageLimit)
	}
}

func TestReadImagesStopsAtTotalBytes(t *testing.T) {
	dir := t.TempDir()
	big := 4 << 20
	paths := []string{
		writeSized(t, dir, "a.bin", big),
		writeSized(t, dir, "b.bin", big),
		writeSized(t, dir, "c.bin", big),
		writeSized(t, dir, "d.bin", big),
	}
	images, failed := readImages(paths)
	if len(images) != 3 || len(failed) != 1 {
		t.Fatalf("read %d images with %d failures, want 3 and 1", len(images), len(failed))
	}
}

func TestReadImageRejectsNonRegularFiles(t *testing.T) {
	if _, err := readImage(t.TempDir()); err == nil {
		t.Fatal("a directory read as an image")
	} else if got := err.Error(); got != errNotRegularFile.Error() {
		t.Fatalf("directory error %q, want %q", got, errNotRegularFile)
	}
	if _, failed := readImages([]string{t.TempDir()}); len(failed) != 1 {
		t.Fatalf("directory drop reported %d failures, want 1", len(failed))
	}
}

type kindClipboard struct {
	kinds []platform.PasteKind
	text  string
	image []byte
}

func (k *kindClipboard) ReadClipboard(_ context.Context, kind platform.PasteKind) (platform.ClipboardContent, error) {
	k.kinds = append(k.kinds, kind)
	if kind == platform.PasteImage {
		return platform.ClipboardContent{Image: k.image, ImageType: "image/png"}, nil
	}
	return platform.ClipboardContent{Text: k.text}, nil
}

func TestPasteImagePrefersImageOverText(t *testing.T) {
	h := idleHarness(t, Host{})
	stub := &kindClipboard{text: "hello", image: pngBytes(t, 2, 2)}
	h.env.Clipboard = func() platform.ClipboardReader { return stub }
	h.view.images[testThreadKey] = true
	h.press("V", key.ModShortcut)
	h.awaitWake()
	if h.text() != "hello" || len(h.dock.draft.chips) != 0 {
		t.Fatalf("ordinary paste inserted %q with chips %+v, want the text and no image", h.text(), h.dock.draft.chips)
	}
	h.press("V", key.ModShortcut|key.ModShift)
	h.awaitWake()
	if len(h.dock.draft.chips) != 1 || h.dock.draft.chips[0].Kind != state.AttachmentImage {
		t.Fatalf("paste image chips = %+v, want one image attachment", h.dock.draft.chips)
	}
	if !slices.Equal(stub.kinds, []platform.PasteKind{platform.PasteOrdinary, platform.PasteImage}) {
		t.Fatalf("paste kinds = %v, want ordinary then image", stub.kinds)
	}
}

func TestPasteImageWithoutImageRefuses(t *testing.T) {
	h := idleHarness(t, Host{}).withClipboard(platform.ClipboardContent{Text: "just words"})
	h.view.images[testThreadKey] = true
	h.press("V", key.ModShortcut|key.ModShift)
	h.awaitWake()
	if h.text() != "" || len(h.dock.draft.chips) != 0 {
		t.Fatalf("paste image without an image inserted %q with chips %+v", h.text(), h.dock.draft.chips)
	}
}
