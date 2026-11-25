//go:build gui

package composer

import (
	"encoding/base64"
	"errors"
	"fmt"
	"image"
	"strings"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const chipThumbnailSize = theme.Space8

type chipImage struct {
	data    []byte
	format  figure.ImageFormat
	details string
	err     error
	job     uint64
	key     figure.PreviewKey
	picture *figure.ImagePaint
}

func (img *chipImage) inspect(c state.Attachment) {
	if img.data == nil {
		_, payload, _ := strings.Cut(c.URL, ";base64,")
		img.data, img.err = base64.StdEncoding.DecodeString(payload)
		if img.err != nil {
			return
		}
	}
	header, err := figure.InspectImage(img.data)
	if err != nil {
		img.err = err
		return
	}
	img.format = header.Format
	img.details = fmt.Sprintf("%s · %s · %d × %d pixels · %s", c.Name, strings.ToUpper(header.Format.String()), header.Width, header.Height, byteSize(len(img.data)))
}

func (d *Dock) chipImage(gtx layout.Context, c state.Attachment) *chipImage {
	if c.Kind != state.AttachmentImage {
		return nil
	}
	img := d.attach.images[c.ID]
	if img == nil {
		img = new(chipImage)
		d.attach.images[c.ID] = img
	}
	if img.format == 0 && img.err == nil {
		img.inspect(c)
	}
	if hub := d.host.Figures; hub != nil && img.err == nil {
		img.advance(gtx, hub.Supervisor, c)
	}
	return img
}

func (img *chipImage) advance(gtx layout.Context, sup *figure.Supervisor, c state.Attachment) {
	switch {
	case img.job == 0:
		img.submit(gtx, sup)
	case img.picture == nil:
		img.collect(gtx, sup, c)
	case !sup.Pixels().HasPreview(img.key):
		img.reload(sup, c)
		gtx.Execute(op.InvalidateCmd{})
	}
}

func (img *chipImage) submit(gtx layout.Context, sup *figure.Supervisor) {
	side := float64(chipThumbnailSize)
	job, _, err := sup.Image(figure.ImageRequest{Format: img.format, Data: img.data, Scale: float64(gtx.Metric.PxPerDp), Viewport: [2]float64{side, side}, Visible: true})
	if errors.Is(err, figure.ErrWaiting) {
		gtx.Execute(op.InvalidateCmd{At: gtx.Now.Add(theme.MotionQuick)})
		return
	}
	img.job, img.err, img.data = job, err, nil
}

func (img *chipImage) collect(gtx layout.Context, sup *figure.Supervisor, c state.Attachment) {
	o, ok := sup.ImageResult(img.job)
	if !ok {
		return
	}
	if o.Err != nil {
		img.err = o.Err
		return
	}
	p, ok := sup.Pixels().Preview(o.Preview)
	if !ok {
		img.reload(sup, c)
		gtx.Execute(op.InvalidateCmd{})
		return
	}
	img.key, img.picture = o.Preview, figure.NewImagePaint(p)
}

func (img *chipImage) reload(sup *figure.Supervisor, c state.Attachment) {
	sup.ReleaseImage(img.job)
	*img = chipImage{}
	img.inspect(c)
}

func (img *chipImage) thumbnail(gtx layout.Context, palette theme.Palette, name string) layout.Dimensions {
	side := dp(gtx, chipThumbnailSize)
	box := gtx.Constraints.Constrain(image.Pt(side, side))
	gtx.Constraints = layout.Exact(box)
	if img.picture == nil {
		giodraw.FillRRect(gtx, palette.Tone, image.Rectangle{Max: box}, dp(gtx, theme.RadiusTag))
		return layout.Dimensions{Size: box}
	}
	return layout.Center.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return img.picture.Layout(gtx, box, "Thumbnail of "+name)
	})
}

func (d *Dock) releaseChip(id string) {
	if img := d.attach.images[id]; img != nil && img.job != 0 {
		d.host.Figures.Supervisor.ReleaseImage(img.job)
	}
	delete(d.attach.images, id)
	delete(d.attach.shown, id)
	delete(d.attach.rows.show, id)
	delete(d.attach.rows.remove, id)
}

func (d *Dock) releaseChips() {
	for id := range d.attach.images {
		d.releaseChip(id)
	}
	clear(d.attach.shown)
	clear(d.attach.rows.show)
	clear(d.attach.rows.remove)
}
