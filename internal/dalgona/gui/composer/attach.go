//go:build gui

package composer

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"gioui.org/io/clipboard"
	"gioui.org/io/transfer"
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	pastedImageName  = "Pasted image"
	composerField    = "composer"
	clipboardTimeout = 30 * time.Second
)

var errNotRegularFile = errors.New("not a regular file; only files can be attached")

var errPasteTimedOut = errors.New("the clipboard did not answer in time; paste again")

var errNoPastedImage = errors.New("the clipboard holds no image; copy an image and use Paste image")

type Host struct {
	PickImages func(ctx context.Context) ([]string, error)
	Wake       func()
	Figures    *figure.Hub
}

func (h Host) wake() {
	if h.Wake != nil {
		h.Wake()
	}
}

func (f frame) clipboard() platform.ClipboardReader {
	if f.Clipboard == nil {
		return nil
	}
	return f.Clipboard()
}

type arrival struct {
	target platform.PasteTarget
	kind   platform.PasteKind
	text   string
	images []imageFile
	failed []string
	err    error
}

type attachments struct {
	arrivals chan arrival
	ctx      context.Context
	stop     context.CancelFunc
	pasteTag bool
	pasting  bool
	target   platform.PasteTarget
	shown    map[string]bool
	images   map[string]*chipImage
	rows     chipRows
}

func (d *Dock) pasteTarget() platform.PasteTarget {
	current := d.editor.Draft()
	return platform.PasteTarget{
		Endpoint:  string(d.key.Endpoint),
		Thread:    d.key.Thread,
		Field:     composerField + "/" + string(d.key.Window),
		Revision:  d.editor.Revision(),
		Selection: current.Selection,
	}
}

func (a *attachments) paste(gtx layout.Context, d *Dock, env frame, kind platform.PasteKind) {
	target, reader := d.pasteTarget(), env.clipboard()
	if reader == nil {
		a.pasting, a.target = true, target
		gtx.Execute(clipboard.ReadCmd{Tag: &a.pasteTag})
		return
	}
	wake := d.host.wake
	go func() {
		ctx, cancel := context.WithTimeout(a.ctx, clipboardTimeout)
		defer cancel()
		content, err := reader.ReadClipboard(ctx, kind)
		next := arrival{target: target, kind: kind, text: content.Text, err: pasteError(ctx, err)}
		switch {
		case len(content.Files) > 0:
			next.images, next.failed = readImages(content.Files)
		case len(content.Image) > 0:
			next.images = []imageFile{{name: pastedImageName, data: content.Image}}
		}
		a.deliver(next, wake)
	}()
}

func pasteError(ctx context.Context, err error) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, context.DeadlineExceeded) || ctx.Err() == context.DeadlineExceeded {
		return errPasteTimedOut
	}
	return err
}

func (a *attachments) deliver(next arrival, wake func()) {
	select {
	case a.arrivals <- next:
		wake()
	case <-a.ctx.Done():
	}
}

func (a *attachments) pick(d *Dock) {
	if d.host.PickImages == nil {
		return
	}
	target, pick, wake := d.pasteTarget(), d.host.PickImages, d.host.wake
	go func() {
		paths, err := pick(context.Background())
		next := arrival{target: target, err: err}
		next.images, next.failed = readImages(paths)
		a.deliver(next, wake)
	}()
}

func (d *Dock) Drop(paths []string) {
	target, attach, wake := d.pasteTarget(), &d.attach, d.host.wake
	go func() {
		images, failed := readImages(paths)
		attach.deliver(arrival{target: target, images: images, failed: failed}, wake)
	}()
}

func readImages(paths []string) ([]imageFile, []string) {
	var images []imageFile
	var failed []string
	total := 0
	for _, p := range paths {
		name := filepath.Base(p)
		if len(images) >= imageLimit || total > imageTotalBytes {
			failed = append(failed, skippedImage(name))
			continue
		}
		data, err := readImage(p)
		if err != nil {
			failed = append(failed, name+": "+err.Error())
			continue
		}
		total += len(data)
		images = append(images, imageFile{name: name, data: data})
	}
	return images, failed
}

func skippedImage(name string) string {
	return fmt.Sprintf("%s: skipped; attachments hold at most %d images and %d MiB per message", name, imageLimit, imageTotalBytes>>20)
}

func readImage(path string) ([]byte, error) {
	info, err := os.Stat(path)
	if err != nil {
		return nil, fmt.Errorf("cannot open the file: %w", errors.Unwrap(err))
	}
	if !info.Mode().IsRegular() {
		return nil, errNotRegularFile
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("cannot open the file: %w", errors.Unwrap(err))
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, imageBytes+1))
	if err != nil {
		return nil, fmt.Errorf("cannot read the file: %w", err)
	}
	if len(data) > imageBytes {
		return nil, errImageTooLarge
	}
	return data, nil
}

func (a *attachments) drain(gtx layout.Context, d *Dock, env frame) {
	if a.pasting {
		a.gioPaste(gtx, d, env)
	}
	for {
		select {
		case next := <-a.arrivals:
			a.apply(d, env, next)
		default:
			return
		}
	}
}

func (a *attachments) gioPaste(gtx layout.Context, d *Dock, env frame) {
	for {
		ev, ok := gtx.Event(platform.PasteFilter(&a.pasteTag))
		if !ok {
			return
		}
		data, isData := ev.(transfer.DataEvent)
		if !isData {
			continue
		}
		a.pasting = false
		text, err := platform.ReadPaste(data)
		a.apply(d, env, arrival{target: a.target, text: text, err: err})
	}
}

func (a *attachments) apply(d *Dock, env frame, next arrival) {
	if err := next.target.Check(d.pasteTarget()); err != nil {
		d.notice.fail(err.Error())
		return
	}
	if next.err != nil {
		d.notice.fail(next.err.Error())
		return
	}
	if next.kind == platform.PasteImage && len(next.images) == 0 {
		d.notice.fail(errNoPastedImage.Error())
		return
	}
	failed := next.failed
	for _, f := range next.images {
		if err := d.attachImage(env, f); err != nil {
			failed = append(failed, f.name+": "+err.Error())
		}
	}
	if next.text != "" {
		d.pasteText(env, next.text)
	}
	if len(failed) > 0 {
		d.notice.fail(strings.Join(failed, "\n"))
	}
}

func (d *Dock) attachImage(env frame, f imageFile) error {
	if env.Thread != (state.ThreadKey{}) {
		if accepts, known := env.view.AcceptsImages(env.Thread); known && !accepts {
			return errImagesRefused
		}
	}
	chip, err := imageChip(d.nextChipID("image"), f)
	if err != nil {
		return err
	}
	if err := admitImage(d.draft.chips, chip); err != nil {
		return err
	}
	d.draft.chips = append(d.draft.chips, chip)
	if d.host.Figures != nil {
		d.attach.images[chip.ID] = &chipImage{data: f.data}
	}
	d.push(env)
	return nil
}

func (d *Dock) pasteText(env frame, text string) {
	p, err := normalizePaste(text)
	if err != nil {
		d.notice.fail(rejectionText(err))
		return
	}
	if p.lines < pasteChipLines {
		if err := d.editor.Replace(d.editor.Draft().Selection, text); err != nil {
			d.notice.reject(rejectionText(err), d.editor.TakeRejected())
		}
		return
	}
	d.draft.chips = append(d.draft.chips, pasteChip(d.nextChipID("paste"), p))
	d.push(env)
}

func (d *Dock) removeChip(env frame, id string) {
	for i, c := range d.draft.chips {
		if c.ID == id {
			d.draft.chips = append(d.draft.chips[:i:i], d.draft.chips[i+1:]...)
			d.releaseChip(id)
			d.push(env)
			return
		}
	}
}
