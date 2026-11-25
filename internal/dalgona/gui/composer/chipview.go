//go:build gui

package composer

import (
	"fmt"
	"strings"
	"unicode/utf8"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const chipsLabel = "Attachments"

type chipRows struct {
	show   map[string]*widget.Clickable
	remove map[string]*widget.Clickable
}

func byteSize(n int) string {
	switch {
	case n < 1<<10:
		return fmt.Sprintf("%d B", n)
	case n < 1<<20:
		return fmt.Sprintf("%d KB", (n+512)>>10)
	}
	return fmt.Sprintf("%.1f MB", float64(n)/(1<<20))
}

func chipLabel(c state.Attachment) string {
	if c.Kind == state.AttachmentImage {
		return "Image · " + c.Name + " · " + byteSize(imageSize(c))
	}
	return c.Name
}

func previewLines(text string, limit int) string {
	cut := 0
	for range limit {
		next := strings.IndexByte(text[cut:], '\n')
		if next < 0 {
			cut = len(text)
			break
		}
		cut += next + 1
	}
	if cut > previewByteLimit {
		cut = previewByteLimit
		for cut > 0 && !utf8.RuneStart(text[cut]) {
			cut--
		}
	}
	if cut == len(text) {
		return text
	}
	return fmt.Sprintf("%s… %d more bytes in the attachment", text[:cut], len(text)-cut)
}

func (r *chipRows) clicker(m *map[string]*widget.Clickable, id string) *widget.Clickable {
	if *m == nil {
		*m = map[string]*widget.Clickable{}
	}
	c := (*m)[id]
	if c == nil {
		c = new(widget.Clickable)
		(*m)[id] = c
	}
	return c
}

func (r *chipRows) update(gtx layout.Context, d *Dock, env frame) {
	for _, c := range d.draft.chips {
		if r.clicker(&r.show, c.ID).Clicked(gtx) {
			d.attach.shown[c.ID] = !d.attach.shown[c.ID]
		}
		if r.clicker(&r.remove, c.ID).Clicked(gtx) {
			d.removeChip(env, c.ID)
			gtx.Execute(focusEditor(d))
			return
		}
	}
}

func (r *chipRows) layout(gtx layout.Context, d *Dock, env frame, look dockLook) layout.Dimensions {
	if len(d.draft.chips) == 0 {
		return layout.Dimensions{}
	}
	items := make([]layout.Widget, 0, len(d.draft.chips))
	for _, c := range d.draft.chips {
		items = append(items, func(gtx layout.Context) layout.Dimensions { return r.chip(gtx, d, env, look, c) })
	}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return column(gtx, theme.Space2, items...) })
	region(gtx, chipsLabel, call, dims)
	return dims
}

func (r *chipRows) chip(gtx layout.Context, d *Dock, env frame, look dockLook, c state.Attachment) layout.Dimensions {
	label := chipLabel(c)
	img := d.chipImage(gtx, c)
	open := d.attach.shown[c.ID]
	toggle := "Show"
	if open {
		toggle = "Hide"
	}
	show := control{click: r.clicker(&r.show, c.ID), label: toggle, describe: label, variant: variantQuiet}
	remove := control{click: r.clicker(&r.remove, c.ID), label: "Remove", describe: label, variant: variantQuiet}
	head := func(gtx layout.Context) layout.Dimensions {
		var thumb layout.Widget
		if img != nil {
			thumb = func(gtx layout.Context) layout.Dimensions { return img.thumbnail(gtx, look.palette, c.Name) }
		}
		return flow(gtx, theme.Space2,
			thumb,
			func(gtx layout.Context) layout.Dimensions {
				return drawText(gtx, env.Shaper, small(look.palette.Text), label)
			},
			func(gtx layout.Context) layout.Dimensions {
				return show.layout(gtx, env.Shaper, look.palette, look.point)
			},
			func(gtx layout.Context) layout.Dimensions {
				return remove.layout(gtx, env.Shaper, look.palette, look.point)
			},
		)
	}
	if img != nil {
		return img.row(gtx, env, look, head, open)
	}
	if !open {
		return head(gtx)
	}
	return column(gtx, theme.Space1, head, func(gtx layout.Context) layout.Dimensions {
		st := small(look.palette.Text)
		st.face = platform.CodeFace
		return drawText(gtx, env.Shaper, st, previewLines(c.Text, previewLineLimit))
	})
}

func (img *chipImage) row(gtx layout.Context, env frame, look dockLook, head layout.Widget, open bool) layout.Dimensions {
	switch {
	case img.err != nil:
		return column(gtx, theme.Space1, head, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, env.Shaper, small(look.palette.FailedText), "This image cannot be shown: "+img.err.Error())
		})
	case open || img.picture == nil:
		return column(gtx, theme.Space1, head, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, env.Shaper, small(look.palette.Dim), img.details)
		})
	default:
		return head(gtx)
	}
}
