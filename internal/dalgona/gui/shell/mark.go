//go:build gui

package shell

import (
	"image"
	"sync"

	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/text"

	"github.com/gosuda/dalgona/assets/brand"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	markDescription = "dalgona: a caramel candy stamped with a crescent moon and the rabbit Gona"
	markSmallBelow  = 24
)

type markLists struct {
	large *figure.List
	small *figure.List
}

var decodeMarks = sync.OnceValues(func() (markLists, error) {
	large, err := figure.DecodeSVG(brand.Mark, figure.DecodeOptions{Profile: figure.ProfileSVG})
	if err != nil {
		return markLists{}, err
	}
	small, err := figure.DecodeSVG(brand.MarkSmall, figure.DecodeOptions{Profile: figure.ProfileSVG})
	return markLists{large: large, small: small}, err
})

type Mark struct {
	Size       theme.Dp
	Decorative bool
}

func (m Mark) Layout(gtx layout.Context, env Env) layout.Dimensions {
	return drawMark(gtx, env.Shaper, env.Theme, m)
}

func drawMark(gtx layout.Context, shaper *text.Shaper, name theme.Name, m Mark) layout.Dimensions {
	marks, err := decodeMarks()
	size := dp(gtx, m.Size)
	call, _ := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		if err != nil {
			st := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeCaption, leading: theme.LeadingTight, color: name.Palette().AccentText, maxLines: 1}
			return paintText(gtx, shaper, st, "dalgona")
		}
		list := marks.large
		if m.Size < markSmallBelow {
			list = marks.small
		}
		canvas := figure.NewCanvas(list, shaper, figure.Faces{Sans: platform.ProseFace, Mono: platform.CodeFace})
		canvas.Zoom = float32(m.Size) / list.Width
		return canvas.Layout(gtx)
	})
	box := image.Pt(size, size)
	defer clip.Rect{Max: box}.Push(gtx.Ops).Pop()
	if !m.Decorative {
		semantic.LabelOp(markDescription).Add(gtx.Ops)
	}
	call.Add(gtx.Ops)
	return layout.Dimensions{Size: box}
}
