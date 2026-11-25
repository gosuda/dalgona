//go:build gui

package shell

import (
	"image"
	"image/color"
	"strings"
	"time"

	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/text"

	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func (t Tag) Layout(gtx layout.Context, env Env) layout.Dimensions {
	return drawTag(gtx, tagDraw{shaper: env.Shaper, name: env.Theme, hue: env.Theme.Point(env.Thread.Thread), tag: t, still: env.Still})
}

type tagDraw struct {
	shaper *text.Shaper
	name   theme.Name
	hue    theme.Hue
	tag    Tag
	still  bool
}

func drawTag(gtx layout.Context, a tagDraw) layout.Dimensions {
	p := a.name.Palette()
	st := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeSmall, leading: theme.LeadingTight, color: p.Dim, maxLines: 1}
	if a.tag.Word == Failed {
		st.weight, st.color = theme.WeightStrong, p.FailedText
		return labelled(gtx, a.tag.Label(), func(gtx layout.Context) layout.Dimensions { return paintText(gtx, a.shaper, st, a.tag.Label()) })
	}
	var fill color.NRGBA
	switch {
	case a.tag.Word == Offline:
		st.color = p.WarningText
	case a.tag.Word == Working && a.tag.Point:
		fill, st.color = a.hue.Fill, a.hue.Ink
	}
	return labelled(gtx, a.tag.Label(), func(gtx layout.Context) layout.Dimensions {
		return tagBox(gtx, fill, func(gtx layout.Context) layout.Dimensions {
			if a.tag.Word != Working {
				return paintText(gtx, a.shaper, st, a.tag.Label())
			}
			return liveContent(gtx, a.shaper, st, a.tag, a.still)
		})
	})
}

func tagBox(gtx layout.Context, fill color.NRGBA, content layout.Widget) layout.Dimensions {
	pad := dp(gtx, theme.Space2)
	inner := gtx
	inner.Constraints.Min = image.Point{}
	inner.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*pad)
	call, dims := giodraw.Record(inner, content)
	size := image.Pt(max(dp(gtx, theme.TagMinWidth), dims.Size.X+2*pad), max(dp(gtx, theme.TagHeight), dims.Size.Y))
	if fill.A > 0 {
		giodraw.FillRRect(gtx, fill, image.Rectangle{Max: size}, dp(gtx, theme.RadiusTag))
	}
	giodraw.Place(gtx, image.Pt(pad, (size.Y-dims.Size.Y)/2), call)
	return layout.Dimensions{Size: size}
}

func liveContent(gtx layout.Context, shaper *text.Shaper, st textStyle, t Tag, still bool) layout.Dimensions {
	clock := liveClock(gtx.Now, t)
	if !still {
		gtx.Execute(op.InvalidateCmd{})
	}
	alpha := float32(1)
	if !still {
		alpha = breath(gtx.Now)
	}
	return row(gtx, theme.Space2,
		func(gtx layout.Context) layout.Dimensions { return dot(gtx, st.color, alpha) },
		func(gtx layout.Context) layout.Dimensions { return paintText(gtx, shaper, st, t.Label()) },
		func(gtx layout.Context) layout.Dimensions { return elapsedField(gtx, shaper, st, clock) },
	)
}

func liveClock(now time.Time, t Tag) string {
	if !t.Retry.IsZero() {
		return formatRetry(t.Retry.Sub(now))
	}
	if t.Started.IsZero() {
		return ""
	}
	return formatDuration(max(0, now.Sub(t.Started)).Truncate(time.Second))
}

func elapsedField(gtx layout.Context, shaper *text.Shaper, st textStyle, clock string) layout.Dimensions {
	widthGtx := gtx
	widthGtx.Constraints.Min = image.Point{}
	_, digits := giodraw.Record(widthGtx, func(gtx layout.Context) layout.Dimensions {
		return paintText(gtx, shaper, st, strings.Repeat("0", theme.ElapsedWidthChars))
	})
	field := gtx
	field.Constraints.Min.X = min(digits.Size.X, gtx.Constraints.Max.X)
	st.align = text.End
	return paintText(field, shaper, st, clock)
}

func dot(gtx layout.Context, c color.NRGBA, alpha float32) layout.Dimensions {
	size := dp(gtx, theme.DotSize)
	c.A = uint8(float32(c.A) * alpha)
	giodraw.FillRRect(gtx, c, image.Rect(0, 0, size, size), size/2)
	return layout.Dimensions{Size: image.Pt(size, size)}
}

func breath(now time.Time) float32 {
	cycle := theme.MotionBreath
	phase := float64(now.UnixNano()%int64(cycle)) / float64(cycle)
	swing := 2 * phase
	if phase > 0.5 {
		swing = 2 - 2*phase
	}
	return 1 - (1-theme.BreathFloor)*easeInOut(float32(swing))
}

func easeInOut(x float32) float32 {
	lo, hi := 0.0, 1.0
	for range 24 {
		mid := (lo + hi) / 2
		if bezier(mid, 0.42, 0.58) < float64(x) {
			lo = mid
		} else {
			hi = mid
		}
	}
	return float32(bezier((lo+hi)/2, 0, 1))
}

func bezier(t, p1, p2 float64) float64 {
	u := 1 - t
	return 3*u*u*t*p1 + 3*u*t*t*p2 + t*t*t
}
