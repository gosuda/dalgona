//go:build gui

package transcript

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"image"
	"image/color"
	"strconv"
	"strings"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const thumbnailHeight = 240

const preparingImageCopy = "Preparing the image for the clipboard."

var (
	errImageDigest       = errors.New("the image bytes do not match their recorded digest; choose Retry")
	errImageSize         = errors.New("the stored image is larger than the 5 MiB attachment limit")
	errImageSourceBudget = errors.New("the window source budget cannot hold another image transfer; finish other transfers and try again")
)

type userImage struct {
	index       int
	data        []byte
	next        uint64
	total       uint64
	loaded      bool
	digest      string
	format      figure.ImageFormat
	asked       uint64
	err         error
	job         uint64
	preview     figure.PreviewKey
	picture     *figure.ImagePaint
	source      image.Point
	retry       widget.Clickable
	toggle      widget.Clickable
	view        widget.Clickable
	save        widget.Clickable
	copy        widget.Clickable
	hidden      bool
	full        bool
	fullJob     uint64
	fullPreview figure.PreviewKey
	fullPicture *figure.ImagePaint
}

type pendingImageCopy struct {
	row   *row
	image *userImage
	meta  attachment
	job   uint64
}

func (t *Transcript) imagesOf(r *row) []*userImage {
	for len(r.images) < len(r.sheet.images) {
		r.images = append(r.images, &userImage{index: len(r.images)})
	}
	return r.images[:len(r.sheet.images)]
}

func (t *Transcript) releaseImages(r *row) {
	hub := t.deps.Figures
	if pending := t.pendingImageCopy; pending != nil && pending.row == r {
		t.cancelImageCopy()
	}
	for _, img := range r.images {
		if hub != nil {
			hub.Supervisor.ReleaseImage(img.job)
			hub.Supervisor.ReleaseImage(img.fullJob)
		}
		if img.picture != nil {
			img.picture.Release()
		}
		if img.fullPicture != nil {
			img.fullPicture.Release()
		}
		img.reset()
	}
	r.images = nil
}

func (r *row) encodedImageBytes(held func([]byte) bool) int {
	n := 0
	for _, img := range r.images {
		if len(img.data) == 0 || !held(img.data) {
			n += cap(img.data)
		}
	}
	return n
}

func (r *row) dropEncodedImages(held func([]byte) bool) int {
	n := 0
	for _, img := range r.images {
		if cap(img.data) == 0 || len(img.data) > 0 && held(img.data) {
			continue
		}
		n += cap(img.data)
		img.data = nil
		img.loaded = false
		img.next, img.total, img.asked = 0, 0, 0
	}
	return n
}

func (img *userImage) step(tv *state.ThreadView, item state.ItemKey, epoch uint64, submit func(state.Intent)) {
	if img.loaded || img.err != nil {
		return
	}
	key := state.ImageKey{Item: item, Index: img.index, Offset: img.next}
	page, ok := tv.Image(key)
	recent := img.asked != 0 && epoch-img.asked < settleEpochs
	switch {
	case ok && page.Loading:
	case ok && page.Err == nil:
		img.accept(page)
		submit(state.ReleaseImagePage{Thread: tv.Key, Key: key})
	case !recent:
		img.requestPage(tv.Key, key, epoch, page.Err, submit)
	}
}

func (img *userImage) requestPage(thread state.ThreadKey, key state.ImageKey, epoch uint64, err error, submit func(state.Intent)) {
	if err != nil {
		img.err = err
		submit(state.ReleaseImagePage{Thread: thread, Key: key})
		return
	}
	img.asked = epoch
	submit(state.ReadImage{Thread: thread, Item: key.Item, Index: key.Index, Offset: key.Offset})
}

func (img *userImage) accept(page state.ImagePage) {
	img.asked = 0
	switch {
	case page.Total > figure.ImageEncodedLimit:
		img.err = errImageSize
		return
	case page.Next <= img.next || page.Next > page.Total:
		img.err = errMismatch
		return
	}
	img.data = append(img.data, page.Data...)
	img.next, img.total, img.digest = page.Next, page.Total, page.Digest
	if img.next < img.total {
		return
	}
	sum := sha256.Sum256(img.data)
	if !strings.EqualFold(hex.EncodeToString(sum[:]), img.digest) {
		img.err = errImageDigest
		return
	}
	img.loaded = true
	header, err := figure.InspectImage(img.data)
	img.format, img.err = header.Format, err
}

func (img *userImage) reset() {
	*img = userImage{index: img.index}
}

func (t *Transcript) render(img *userImage, gtx layout.Context, column int) {
	hub := t.deps.Figures
	switch {
	case !img.loaded || img.err != nil:
		return
	case img.job == 0:
		t.submitImage(img, gtx, column)
	case img.picture == nil || !img.picture.Live():
		t.collectImage(img, hub)
	}
	t.updateFullImage(img, gtx, column)
}

func (t *Transcript) updateFullImage(img *userImage, gtx layout.Context, column int) {
	if img.fullPicture != nil && (!img.full || !img.fullPicture.Live()) {
		t.deps.Figures.Supervisor.ReleaseImage(img.fullJob)
		img.fullPicture.Release()
		img.fullJob, img.fullPicture = 0, nil
		img.fullPreview = figure.PreviewKey{}
	}
	if img.full {
		t.renderFull(img, gtx, column)
	}
}

func (t *Transcript) submitImage(img *userImage, gtx layout.Context, column int) {
	scale := float64(gtx.Metric.PxPerDp)
	img.job, img.err = t.requestImagePreview(img, [2]float64{float64(column) / scale, thumbnailHeight}, scale)
}

func (t *Transcript) requestImagePreview(img *userImage, viewport [2]float64, scale float64) (uint64, error) {
	release, ok := t.leaseImageSource(img.data)
	if !ok {
		return 0, errImageSourceBudget
	}
	req := figure.ImageRequest{Format: img.format, Data: img.data, Scale: scale, Viewport: viewport, Visible: true, ReleaseSource: release}
	id, _, err := t.deps.Figures.Supervisor.Image(req)
	if err != nil {
		release()
	}
	return id, err
}

func (t *Transcript) renderFull(img *userImage, gtx layout.Context, column int) {
	hub := t.deps.Figures
	switch {
	case img.fullJob == 0:
		scale := float64(gtx.Metric.PxPerDp)
		width := float64(column) / scale
		img.fullJob, img.err = t.requestImagePreview(img, [2]float64{width, width}, scale)
		if img.err != nil {
			img.full = false
		}
	case img.fullPicture == nil:
		o, ok := hub.Supervisor.ImageResult(img.fullJob)
		if !ok {
			return
		}
		if o.Err != nil {
			img.err = o.Err
			img.full = false
			return
		}
		img.fullPreview = o.Preview
		pic, ok := hub.Supervisor.Pixels().Paint(img.fullPreview)
		if !ok {
			hub.Supervisor.ReleaseImage(img.fullJob)
			img.fullJob = 0
			return
		}
		img.fullPicture = pic
	case !hub.Supervisor.Pixels().HasPreview(img.fullPreview):
		hub.Supervisor.ReleaseImage(img.fullJob)
		if img.fullPicture != nil {
			img.fullPicture.Release()
		}
		img.fullJob, img.fullPicture = 0, nil
	}
}

func (t *Transcript) collectImage(img *userImage, hub *figure.Hub) {
	o, ok := hub.Supervisor.ImageResult(img.job)
	if !ok {
		return
	}
	if o.Err != nil {
		img.err = o.Err
		return
	}
	img.preview = o.Preview
	pic, ok := hub.Supervisor.Pixels().Paint(img.preview)
	if !ok {
		hub.Supervisor.ReleaseImage(img.job)
		img.job = 0
		if img.picture != nil {
			img.picture.Release()
			img.picture = nil
		}
		return
	}
	if img.picture != nil {
		img.picture.Release()
	}
	img.picture = pic
	img.source = pic.SourceSize()
}

func (t *Transcript) userImages(gtx layout.Context, env shell.Env, r *row) []layout.FlexChild {
	images := t.imagesOf(r)
	if len(images) == 0 {
		return nil
	}
	tv, _ := env.View.Thread(r.entry.Thread)
	out := make([]layout.FlexChild, 0, 2*len(images))
	for i, img := range images {
		meta := r.sheet.images[i]
		if tv != nil && t.deps.Figures != nil {
			img.step(tv, r.entry.Item.Key, t.loader.epoch, env.Submit)
			t.render(img, gtx, gtx.Constraints.Max.X)
			out = append(out, layout.Rigid(t.previewToggle(meta, img)))
			out = t.loadedImageActions(out, r, img)
		}
		out = append(out, layout.Rigid(func(gtx layout.Context) layout.Dimensions { return t.thumbnail(gtx, img) }))
		out = append(out, layout.Rigid(t.caption(env, imageCaption(meta, img), imageColor(t.palette, img))))
		if img.err != nil {
			out = append(out, layout.Rigid(t.imageRetry(img)))
		}
	}
	return out
}

func (t *Transcript) loadedImageActions(out []layout.FlexChild, r *row, img *userImage) []layout.FlexChild {
	if !img.loaded || len(img.data) == 0 {
		return out
	}
	meta := r.sheet.images[img.index]
	if img.err == nil {
		out = append(out, layout.Rigid(t.fullToggle(meta, img)))
	}
	if t.deps.Save != nil {
		out = append(out, layout.Rigid(t.imageSave(meta, img)))
	}
	if img.format != 0 && t.deps.CopyImage != nil {
		out = append(out, layout.Rigid(t.imageCopy(r, meta, img)))
	}
	return out
}

func (t *Transcript) previewToggle(meta attachment, img *userImage) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		if img.toggle.Clicked(gtx) {
			img.hidden = !img.hidden
		}
		dims := t.ctrl.button(access.Hide(gtx), &img.toggle, previewAction(meta, img.hidden), false)
		access.Place(gtx, image.Rectangle{Max: dims.Size}, &img.toggle)
		return dims
	}
}

func previewAction(meta attachment, hidden bool) string {
	if hidden {
		return "Show preview for " + meta.name()
	}
	return "Hide preview for " + meta.name()
}

func (t *Transcript) imageRetry(img *userImage) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		if img.retry.Clicked(gtx) {
			if pending := t.pendingImageCopy; pending != nil && pending.image == img {
				t.cancelImageCopy()
			}
			t.deps.Figures.Supervisor.ReleaseImage(img.job)
			t.deps.Figures.Supervisor.ReleaseImage(img.fullJob)
			img.reset()
		}
		return t.ctrl.button(gtx, &img.retry, "Retry image "+strconv.Itoa(img.index+1), false)
	}
}

func (t *Transcript) fullToggle(meta attachment, img *userImage) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		if img.view.Clicked(gtx) {
			img.full = !img.full
		}
		name := "View full size for " + meta.name()
		if img.full {
			name = "Show thumbnail for " + meta.name()
		}
		return t.ctrl.button(gtx, &img.view, name, false)
	}
}

func (t *Transcript) imageSave(meta attachment, img *userImage) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		if img.save.Clicked(gtx) && len(img.data) > 0 {
			release, ok := t.leaseImageSource(img.data)
			if ok {
				t.startImageSave("image "+strconv.Itoa(img.index+1), meta.name(), img.data, release)
			} else {
				t.notice = "Save image failed: " + errImageSourceBudget.Error()
			}
		}
		return t.ctrl.button(gtx, &img.save, "Save image "+strconv.Itoa(img.index+1), false)
	}
}

func (t *Transcript) imageCopy(r *row, meta attachment, img *userImage) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		if img.copy.Clicked(gtx) && t.pendingImageCopy == nil {
			t.startImageCopy(r, meta, img)
		}
		return t.ctrl.button(gtx, &img.copy, "Copy image "+strconv.Itoa(img.index+1), t.pendingImageCopy != nil)
	}
}

func (t *Transcript) startImageCopy(r *row, meta attachment, img *userImage) {
	release, ok := t.leaseImageSource(img.data)
	if !ok {
		t.notice = "Copy image failed: " + errImageSourceBudget.Error()
		return
	}
	offer, err := t.deps.CopyImage()
	if err != nil {
		release()
		t.notice = "Copy image failed: " + err.Error()
		return
	}
	job, err := t.deps.Figures.Supervisor.ExportClipboardImage(img.format, img.data, offer, release)
	if err != nil {
		release()
		offer.Close()
		t.notice = "Copy image failed: " + err.Error()
		return
	}
	t.pendingImageCopy = &pendingImageCopy{row: r, image: img, meta: meta, job: job}
	t.notice = preparingImageCopy
}

func (t *Transcript) cancelImageCopy() {
	if pending := t.pendingImageCopy; pending != nil {
		t.pendingImageCopy = nil
		t.deps.Figures.Supervisor.Cancel(pending.job)
		if t.notice == preparingImageCopy {
			t.notice = ""
		}
	}
}

func (t *Transcript) finishImageCopy(p *pane) {
	pending := t.pendingImageCopy
	if pending == nil {
		return
	}
	if !pending.matches(p) {
		t.cancelImageCopy()
		return
	}
	offer, err, ready := t.deps.Figures.Supervisor.TakeClipboardImage(pending.job)
	if !ready {
		return
	}
	t.pendingImageCopy = nil
	if err == nil {
		defer offer.Close()
		err = offer.Publish()
	}
	if err != nil {
		t.notice = "Copy image failed: " + err.Error()
		return
	}
	t.notice = "Copied image " + strconv.Itoa(pending.image.index+1) + "."
}

func (c *pendingImageCopy) matches(p *pane) bool {
	r, i := c.row, c.image.index
	return p.rows[r.id] == r && i < len(r.sheet.images) && r.sheet.images[i] == c.meta && i < len(r.images) && r.images[i] == c.image && c.image.loaded
}

func imageCaption(meta attachment, img *userImage) string {
	parts := []string{meta.name()}
	if meta.Mime != "" {
		parts = append(parts, meta.Mime)
	}
	if meta.Bytes > 0 {
		parts = append(parts, byteSize(uint64(meta.Bytes)))
	}
	if img.source.X > 0 {
		parts = append(parts, strconv.Itoa(img.source.X)+" × "+strconv.Itoa(img.source.Y)+" pixels")
	}
	if img.err != nil {
		parts = append(parts, "the image could not be shown: "+img.err.Error())
	}
	return strings.Join(parts, " · ")
}

func imageColor(p theme.Palette, img *userImage) color.NRGBA {
	if img.err != nil {
		return p.FailedText
	}
	return p.Dim
}

func (t *Transcript) thumbnail(gtx layout.Context, img *userImage) layout.Dimensions {
	if img.hidden {
		return layout.Dimensions{}
	}
	if img.full {
		if img.fullPicture == nil {
			return layout.Dimensions{}
		}
		limit := image.Pt(gtx.Constraints.Max.X, 3*dp(gtx, thumbnailHeight))
		dims := img.fullPicture.Layout(gtx, limit, "Image "+strconv.Itoa(img.index+1)+" full size")
		dims.Size.Y += dp(gtx, theme.Space1)
		return dims
	}
	if img.picture == nil {
		return layout.Dimensions{}
	}
	limit := image.Pt(gtx.Constraints.Max.X, dp(gtx, thumbnailHeight))
	dims := img.picture.Layout(gtx, limit, "Image "+strconv.Itoa(img.index+1))
	dims.Size.Y += dp(gtx, theme.Space1)
	return dims
}
