//go:build gui

package panel

import (
	"image"
	"slices"

	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	scrimAlpha   = 0xcc
	compactSpan  = theme.Dp(256)
	clearanceCut = 4
)

type Frame struct {
	body    layout.List
	dismiss Button
	ring    []event.Tag
	focused event.Tag
	settle  bool
}

type Content struct {
	Title     string
	Rows      int
	Row       layout.ListElement
	Actions   []layout.Widget
	Dismiss   string
	OnDismiss func()
}

func (f *Frame) Reset() {
	f.body = layout.List{Axis: layout.Vertical}
	f.dismiss = Button{}
	f.focused = nil
	f.settle = true
}

func (f *Frame) Focusable(tag event.Tag) {
	f.ring = append(f.ring, tag)
}

func (f *Frame) Layout(gtx layout.Context, env shell.Env, c Content) layout.Dimensions {
	f.body.Axis = layout.Vertical
	f.traverse(gtx)
	f.ring = f.ring[:0]
	if f.dismiss.Clicked(gtx) {
		dismiss(env, c)
	}
	size := gtx.Constraints.Max
	p := env.Theme.Palette()
	scrim := p.Canvas
	scrim.A = scrimAlpha
	giodraw.FillRect(gtx, scrim, image.Rectangle{Max: size})
	access.Group(gtx, platform.SemanticNode{ID: "panel", Role: platform.RoleDialog, Name: c.Title}, access.Actions{}, func(gtx layout.Context) layout.Dimensions {
		f.card(gtx, env, c)
		return layout.Dimensions{Size: size}
	})
	f.escape(gtx, env, c)
	f.restore(gtx)
	return layout.Dimensions{Size: size}
}

func dismiss(env shell.Env, c Content) {
	if c.OnDismiss != nil {
		c.OnDismiss()
		return
	}
	env.Close()
}

func clearance(gtx layout.Context, size image.Point) int {
	spare := (min(size.X, size.Y) - dp(gtx, compactSpan)) / clearanceCut
	return min(max(spare, dp(gtx, theme.Space2)), dp(gtx, theme.Space8))
}

type cardParts struct {
	title, body, footer           op.CallOp
	titleSize, bodySize, footSize image.Point
}

func (f *Frame) card(gtx layout.Context, env shell.Env, c Content) {
	size := gtx.Constraints.Max
	margin := clearance(gtx, size)
	pad := dp(gtx, theme.Space4)
	spacing := dp(gtx, theme.Space3)
	width := min(dp(gtx, theme.ModalMaxWidth), size.X-2*margin)
	inner := gtx
	inner.Constraints = layout.Constraints{Max: image.Pt(max(0, width-2*pad), size.Y)}
	var parts cardParts
	parts.title, parts.titleSize = recordSize(inner, func(gtx layout.Context) layout.Dimensions {
		return drawText(gtx, env.Shaper, titleText(env.Theme.Palette()), c.Title)
	})
	room := size.Y - 2*margin - 2*pad - parts.titleSize.Y - 2*spacing
	start := len(f.ring)
	parts.footer, parts.footSize = recordSize(inner, func(gtx layout.Context) layout.Dimensions { return f.footer(gtx, env, c) })
	split := len(f.ring)
	parts.body, parts.bodySize = f.layoutBody(inner, c, room-parts.footSize.Y)
	f.footerLast(start, split)
	height := 2*pad + parts.titleSize.Y + parts.bodySize.Y + parts.footSize.Y + 2*spacing
	origin := image.Pt((size.X-width)/2, max(0, (size.Y-height)/2))
	defer op.Offset(origin).Push(gtx.Ops).Pop()
	p := env.Theme.Palette()
	box := image.Rectangle{Max: image.Pt(width, height)}
	radius := dp(gtx, theme.RadiusCard)
	giodraw.FillRRect(gtx, p.Surface, box, radius)
	giodraw.StrokeRRect(gtx, p.Line, box, radius, dp(gtx, theme.BorderWidth))
	y := pad
	giodraw.Place(gtx, image.Pt(pad, y), parts.title)
	y += parts.titleSize.Y + spacing
	giodraw.Place(gtx, image.Pt(pad, y), parts.body)
	y += parts.bodySize.Y + spacing
	giodraw.Place(gtx, image.Pt(pad+max(0, width-2*pad-parts.footSize.X), y), parts.footer)
}

func recordSize(gtx layout.Context, w layout.Widget) (op.CallOp, image.Point) {
	call, dims := giodraw.Record(gtx, w)
	return call, dims.Size
}

func (f *Frame) layoutBody(gtx layout.Context, c Content, room int) (op.CallOp, image.Point) {
	gtx.Constraints = layout.Constraints{Min: image.Pt(gtx.Constraints.Max.X, 0), Max: image.Pt(gtx.Constraints.Max.X, max(0, room))}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return f.body.Layout(gtx, c.Rows, func(gtx layout.Context, i int) layout.Dimensions {
			return access.Scrollable(gtx, &f.body, i, func(gtx layout.Context) layout.Dimensions {
				if i == 0 {
					return c.Row(gtx, i)
				}
				return layout.Inset{Top: dpUnit(theme.Space3)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions { return c.Row(gtx, i) })
			})
		})
	})
	return call, dims.Size
}

func (f *Frame) footerLast(start, split int) {
	slices.Reverse(f.ring[start:split])
	slices.Reverse(f.ring[split:])
	slices.Reverse(f.ring[start:])
}

func (f *Frame) footer(gtx layout.Context, env shell.Env, c Content) layout.Dimensions {
	label := c.Dismiss
	if label == "" {
		label = "Close"
	}
	widgets := append(slices.Clip(c.Actions), func(gtx layout.Context) layout.Dimensions {
		return f.dismiss.Layout(gtx, f, env, label, Quiet)
	})
	return flow(gtx, widgets)
}

func flow(gtx layout.Context, widgets []layout.Widget) layout.Dimensions {
	spacing := dp(gtx, theme.Space3)
	limit := gtx.Constraints.Max.X
	child := gtx
	child.Constraints.Min = image.Point{}
	type piece struct {
		call op.CallOp
		at   image.Point
	}
	pieces := make([]piece, 0, len(widgets))
	var x, y, line, width int
	for _, w := range widgets {
		call, dims := giodraw.Record(child, w)
		if x > 0 && x+dims.Size.X > limit {
			x, y, line = 0, y+line+spacing, 0
		}
		pieces = append(pieces, piece{call: call, at: image.Pt(x, y)})
		x += dims.Size.X + spacing
		line = max(line, dims.Size.Y)
		width = max(width, x-spacing)
	}
	for _, pc := range pieces {
		giodraw.Place(gtx, pc.at, pc.call)
	}
	return layout.Dimensions{Size: image.Pt(width, y+line)}
}

func (f *Frame) traverse(gtx layout.Context) {
	for {
		e, ok := gtx.Event(key.Filter{Name: key.NameTab, Optional: key.ModShift})
		if !ok {
			return
		}
		press, isKey := e.(key.Event)
		if !isKey || press.State != key.Press {
			continue
		}
		step := 1
		if press.Modifiers.Contain(key.ModShift) {
			step = -1
		}
		f.Step(gtx, step)
	}
}

func (f *Frame) Step(gtx layout.Context, step int) {
	if len(f.ring) > 0 {
		gtx.Execute(key.FocusCmd{Tag: f.ring[f.next(gtx, step)]})
	}
}

func (f *Frame) next(gtx layout.Context, step int) int {
	at := slices.IndexFunc(f.ring, gtx.Focused)
	if at < 0 {
		if step > 0 {
			return 0
		}
		return len(f.ring) - 1
	}
	return (at + step + len(f.ring)) % len(f.ring)
}

func (f *Frame) escape(gtx layout.Context, env shell.Env, c Content) {
	for {
		e, ok := gtx.Event(key.Filter{Name: key.NameEscape})
		if !ok {
			return
		}
		if press, isKey := e.(key.Event); isKey && press.State == key.Press {
			dismiss(env, c)
		}
	}
}

func (f *Frame) restore(gtx layout.Context) {
	if len(f.ring) == 0 {
		return
	}
	lost := f.focused != nil && !slices.Contains(f.ring, f.focused)
	if f.settle || lost {
		f.settle = false
		gtx.Execute(key.FocusCmd{Tag: f.ring[0]})
		f.focused = f.ring[0]
		return
	}
	if at := slices.IndexFunc(f.ring, gtx.Focused); at >= 0 {
		f.focused = f.ring[at]
	}
}
