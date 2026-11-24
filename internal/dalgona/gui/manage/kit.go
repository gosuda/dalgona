//go:build gui

package manage

import (
	"image"
	"image/color"

	giofont "gioui.org/font"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type tone uint8

const (
	toneBody tone = iota
	toneDim
	toneHeading
	toneCode
	toneFailed
	toneWarning
	toneAdded
)

type variant uint8

const (
	quiet variant = iota
	secondary
	pointed
	destructive
)

type action struct {
	click   *widget.Clickable
	label   string
	variant variant
	off     bool
}

func dp(gtx layout.Context, v theme.Dp) int { return gtx.Dp(unit.Dp(v)) }

func focusRing(gtx layout.Context, c color.NRGBA, size image.Point, radius int) {
	grow := dp(gtx, theme.FocusOffset) + dp(gtx, theme.FocusWidth)/2
	r := image.Rectangle{Min: image.Pt(-grow, -grow), Max: size.Add(image.Pt(grow, grow))}
	giodraw.StrokeRRect(gtx, c, r, radius+grow, dp(gtx, theme.FocusWidth))
}

type ink struct {
	face    giofont.Typeface
	weight  theme.Weight
	size    theme.Sp
	leading float32
	color   color.NRGBA
}

func inkFor(p theme.Palette, t tone) ink {
	body := ink{face: platform.ProseFace, weight: theme.WeightText, size: theme.SizeBody, leading: theme.LeadingBody, color: p.Text}
	switch t {
	case toneDim:
		body.size, body.leading, body.color = theme.SizeSmall, theme.LeadingDescription, p.Dim
	case toneHeading:
		body = ink{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeLabel, leading: theme.LeadingLabel, color: p.Text}
	case toneCode:
		body = ink{face: platform.CodeFace, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingCode, color: p.Text}
	case toneFailed:
		body.weight, body.color = theme.WeightStrong, p.FailedText
	case toneWarning:
		body.color = p.WarningText
	case toneAdded:
		body.color = p.AddedText
	case toneBody:
	}
	return body
}

func paintText(gtx layout.Context, shaper *text.Shaper, in ink, s string, lines int) layout.Dimensions {
	l := widget.Label{MaxLines: lines, LineHeightScale: in.leading}
	f := giofont.Font{Typeface: in.face, Weight: platform.FontWeight(in.weight)}
	return l.Layout(gtx, shaper, f, unit.Sp(in.size), s, giodraw.Material(gtx, in.color))
}

func say(gtx layout.Context, env scene, t tone, s string) layout.Dimensions {
	gtx.Constraints.Min = image.Point{}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return paintText(gtx, env.Shaper, inkFor(env.Theme.Palette(), t), s, 0)
	})
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	call.Add(gtx.Ops)
	return dims
}

func sayer(env scene, t tone, s string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions { return say(gtx, env, t, s) }
}

type buttonStyle struct {
	fill   color.NRGBA
	edge   color.NRGBA
	label  color.NRGBA
	inline theme.Dp
}

func buttonLook(p theme.Palette, hue theme.Hue, a action) buttonStyle {
	switch {
	case a.off:
		return buttonStyle{fill: p.Tone, edge: p.Line, label: p.Dim, inline: theme.Space5}
	case a.variant == pointed:
		return buttonStyle{fill: hue.Fill, edge: hue.Fill, label: hue.Ink, inline: theme.Space5}
	case a.variant == secondary:
		return buttonStyle{fill: p.Tone, edge: p.Strong, label: p.Text, inline: theme.Space5}
	case a.variant == destructive:
		return buttonStyle{fill: color.NRGBA{}, edge: p.FailedText, label: p.FailedText, inline: theme.Space5}
	}
	return buttonStyle{label: p.Dim, inline: theme.Space3}
}

func button(gtx layout.Context, env scene, a action) layout.Dimensions {
	p := env.Theme.Palette()
	style := buttonLook(p, env.Theme.Point(env.Thread.Thread), a)
	gtx.Constraints.Min = image.Point{}
	if !a.off {
		env.owner.frame.Focusable(a.click)
	}
	return a.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(a.label).Add(gtx.Ops)
		semantic.EnabledOp(!a.off).Add(gtx.Ops)
		in := ink{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeLabel, leading: theme.LeadingTight, color: style.label}
		pad := dp(gtx, style.inline)
		textGtx := gtx
		textGtx.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*pad)
		call, dims := giodraw.Record(textGtx, func(gtx layout.Context) layout.Dimensions { return paintText(gtx, env.Shaper, in, a.label, 1) })
		size := image.Pt(dims.Size.X+2*pad, max(dp(gtx, theme.ControlHeight), dims.Size.Y))
		radius := dp(gtx, theme.RadiusControl)
		bounds := image.Rectangle{Max: size}
		if style.fill.A > 0 {
			giodraw.FillRRect(gtx, style.fill, bounds, radius)
		}
		if style.edge.A > 0 {
			giodraw.StrokeRRect(gtx, style.edge, bounds, radius, dp(gtx, theme.BorderWidth))
		}
		giodraw.Place(gtx, image.Pt(pad, (size.Y-dims.Size.Y)/2), call)
		access.Add(gtx, bounds, platform.SemanticNode{Role: platform.RoleButton, Name: a.label, Disabled: a.off, Action: "activate"}, access.Actions{Click: a.click})
		if gtx.Focused(a.click) {
			focusRing(gtx, p.Text, size, radius)
		}
		return layout.Dimensions{Size: size}
	})
}

func clicked(gtx layout.Context, c *widget.Clickable, off bool) bool {
	hit := c.Clicked(gtx)
	return hit && !off
}

func flow(gtx layout.Context, gap theme.Dp, items ...layout.Widget) layout.Dimensions {
	space := dp(gtx, gap)
	room := gtx.Constraints.Max.X
	child := gtx
	child.Constraints.Min = image.Point{}
	x, y, line, width := 0, 0, 0, 0
	for _, w := range items {
		if w == nil {
			continue
		}
		call, dims := giodraw.Record(child, w)
		if x > 0 && x+dims.Size.X > room {
			x, y, line = 0, y+line+space, 0
		}
		giodraw.Place(gtx, image.Pt(x, y), call)
		x += dims.Size.X + space
		line = max(line, dims.Size.Y)
		width = max(width, x-space)
	}
	return layout.Dimensions{Size: image.Pt(width, y+line)}
}

func stack(gtx layout.Context, gap theme.Dp, items ...layout.Widget) layout.Dimensions {
	space := dp(gtx, gap)
	child := gtx
	child.Constraints.Min = image.Point{}
	child.Constraints.Max.Y = 1 << 24
	y, width := 0, 0
	for _, w := range items {
		if w == nil {
			continue
		}
		if y > 0 {
			y += space
		}
		call, dims := giodraw.Record(child, w)
		giodraw.Place(gtx, image.Pt(0, y), call)
		y += dims.Size.Y
		width = max(width, dims.Size.X)
	}
	return layout.Dimensions{Size: image.Pt(width, y)}
}

func buttons(env scene, actions ...action) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		items := make([]layout.Widget, 0, len(actions))
		for _, a := range actions {
			if a.click == nil {
				continue
			}
			items = append(items, func(gtx layout.Context) layout.Dimensions { return button(gtx, env, a) })
		}
		return flow(gtx, theme.Space3, items...)
	}
}

type toggle struct {
	click widget.Clickable
	on    bool
}

func (t *toggle) update(gtx layout.Context, off bool) bool {
	if clicked(gtx, &t.click, off) {
		t.on = !t.on
		return true
	}
	return false
}

func (t *toggle) layout(gtx layout.Context, env scene, label string, off bool) layout.Dimensions {
	p := env.Theme.Palette()
	gtx.Constraints.Min = image.Point{}
	if !off {
		env.owner.frame.Focusable(&t.click)
	}
	return t.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.CheckBox.Add(gtx.Ops)
		semantic.LabelOp(label).Add(gtx.Ops)
		semantic.SelectedOp(t.on).Add(gtx.Ops)
		semantic.EnabledOp(!off).Add(gtx.Ops)
		box := dp(gtx, theme.OptionMarkSize)
		radius := dp(gtx, theme.RadiusEdit)
		mark := image.Rectangle{Max: image.Pt(box, box)}
		giodraw.StrokeRRect(gtx, p.Strong, mark, radius, dp(gtx, theme.BorderWidth))
		if t.on {
			inset := dp(gtx, theme.Space1)
			giodraw.FillRRect(gtx, p.Text, mark.Inset(inset), radius/2)
		}
		gap := dp(gtx, theme.Space2)
		call, dims := giodraw.Record(gtx, sayer(env, toneBody, label))
		height := max(box, dims.Size.Y, dp(gtx, theme.TargetMin))
		giodraw.Place(gtx, image.Pt(box+gap, (height-dims.Size.Y)/2), call)
		size := image.Pt(box+gap+dims.Size.X, height)
		access.Add(gtx, image.Rectangle{Max: size}, platform.SemanticNode{Role: platform.RoleCheckBox, Name: label, Checked: t.on, Disabled: off, Action: "toggle"}, access.Actions{Click: &t.click})
		if gtx.Focused(&t.click) {
			focusRing(gtx, p.Text, size, radius)
		}
		return layout.Dimensions{Size: size}
	})
}

type field struct {
	editor  *platform.Editor
	policy  inputtext.Policy
	secret  bool
	refusal string
	escaped bool
}

func newField(policy inputtext.Policy, secret bool) *field {
	return &field{policy: policy, secret: secret}
}

func (f *field) ensure() *platform.Editor {
	if f.editor != nil {
		return f.editor
	}
	editor, err := platform.NewEditor(f.policy, inputtext.Draft{})
	if err != nil {
		f.refusal = refusalText(err)
		return nil
	}
	if f.secret {
		editor.Mask = '•'
	}
	f.editor = editor
	return editor
}

func (f *field) text() string {
	if f.editor == nil {
		return ""
	}
	return f.editor.Draft().Text
}

func (f *field) set(s string) {
	e := f.ensure()
	if e == nil {
		return
	}
	f.refusal = ""
	if err := e.SetDraft(inputtext.Draft{Text: s, Selection: inputtext.Range{Start: len(s), End: len(s)}}); err != nil {
		f.refusal = refusalText(err)
	}
}

func (f *field) update(gtx layout.Context) (changed, submitted bool) {
	e := f.ensure()
	for e != nil {
		ev, ok := e.Update(gtx)
		if !ok {
			break
		}
		switch ev.Kind {
		case platform.EditorChange:
			f.refusal, changed = "", true
		case platform.EditorRejected:
			e.TakeRejected()
			f.refusal = refusalText(ev.Err)
		case platform.EditorSubmit:
			submitted = f.enter(e) || submitted
		case platform.EditorCancel:
			f.escaped = true
		case platform.EditorKey:
		}
	}
	return changed, submitted
}

func (f *field) enter(e *platform.Editor) bool {
	if f.policy.Lines != inputtext.MultiLine {
		return true
	}
	if err := e.Replace(e.Draft().Selection, "\n"); err != nil {
		e.TakeRejected()
		f.refusal = refusalText(err)
	}
	return false
}

func refusalText(err error) string {
	return "Not accepted: " + err.Error() + ". The previous text is kept."
}

func (f *field) layout(gtx layout.Context, env scene, label string, off bool) layout.Dimensions {
	if f.escaped {
		f.escaped, env.owner.escaped = false, true
	}
	p := env.Theme.Palette()
	gtx.Constraints.Min = image.Point{}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return stack(gtx, theme.Space1,
			func(gtx layout.Context) layout.Dimensions {
				return paintText(gtx, env.Shaper, inkFor(p, toneDim), label, 0)
			},
			func(gtx layout.Context) layout.Dimensions { return f.box(gtx, env, p, off) })
	})
	field := func(gtx layout.Context) layout.Dimensions {
		defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
		semantic.LabelOp(label).Add(gtx.Ops)
		call.Add(gtx.Ops)
		f.registerField(gtx, dims.Size, label, off)
		return dims
	}
	return stack(gtx, theme.Space1, field, f.refusalLine(env))
}

func (f *field) registerField(gtx layout.Context, size image.Point, label string, off bool) {
	if f.editor == nil {
		return
	}
	role := platform.RoleTextField
	if f.secret {
		role = platform.RoleSecureField
	}
	n := platform.SemanticNode{ID: platform.NodeID(access.Path("field", label)), Role: role, Name: label, Description: f.refusal, Disabled: off, Action: "edit"}
	access.Add(gtx, image.Rectangle{Max: size}, n, access.Actions{Editor: f.editor, Rejected: func(err error) { f.refusal = refusalText(err) }})
}

func (f *field) refusalLine(env scene) layout.Widget {
	if f.refusal == "" {
		return nil
	}
	return sayer(env, toneFailed, f.refusal)
}

func (f *field) box(gtx layout.Context, env scene, p theme.Palette, off bool) layout.Dimensions {
	e := f.ensure()
	if e == nil {
		return layout.Dimensions{}
	}
	e.Clipboard, e.Wake = env.Clipboard, env.Wake
	pad := dp(gtx, theme.Space2)
	inline := dp(gtx, theme.Space3)
	width := gtx.Constraints.Max.X
	inner := gtx
	inner.Constraints = layout.Constraints{Min: image.Pt(max(0, width-2*inline), 0), Max: image.Pt(max(0, width-2*inline), 1<<24)}
	if f.policy.Lines == inputtext.SingleLine {
		inner.Constraints.Max.Y = dp(gtx, theme.ControlHeight)
	}
	if off {
		inner = inner.Disabled()
	} else {
		env.owner.frame.Focusable(e.Tag())
	}
	in := inkFor(p, toneBody)
	call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		return e.Layout(gtx, platform.EditorStyle{Shaper: env.Shaper, Font: giofont.Font{Typeface: in.face}, Size: unit.Sp(theme.InputTextSize), Text: giodraw.Material(gtx, p.Text), Selection: giodraw.Material(gtx, p.Tone)})
	})
	size := image.Pt(width, max(dims.Size.Y+2*pad, dp(gtx, theme.ControlHeight)))
	radius := dp(gtx, theme.RadiusControl)
	bounds := image.Rectangle{Max: size}
	giodraw.FillRRect(gtx, p.Surface, bounds, radius)
	giodraw.StrokeRRect(gtx, p.Strong, bounds, radius, dp(gtx, theme.BorderWidth))
	giodraw.Place(gtx, image.Pt(inline, (size.Y-dims.Size.Y)/2), call)
	if gtx.Focused(e.Tag()) {
		focusRing(gtx, p.Text, size, radius)
	}
	return layout.Dimensions{Size: size}
}

type choice struct {
	clicks []widget.Clickable
}

func (c *choice) grow(n int) {
	if len(c.clicks) < n {
		c.clicks = append(c.clicks, make([]widget.Clickable, n-len(c.clicks))...)
	}
}

func (c *choice) picked(gtx layout.Context, n int) int {
	c.grow(n)
	at := -1
	for i := range n {
		if c.clicks[i].Clicked(gtx) {
			at = i
		}
	}
	return at
}

type optionSpec struct {
	click    *widget.Clickable
	label    string
	selected bool
	content  layout.Widget
}

func option(gtx layout.Context, env scene, spec optionSpec) layout.Dimensions {
	p := env.Theme.Palette()
	gtx.Constraints.Min = image.Point{}
	env.owner.frame.Focusable(spec.click)
	return spec.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		semantic.Button.Add(gtx.Ops)
		semantic.LabelOp(spec.label).Add(gtx.Ops)
		semantic.SelectedOp(spec.selected).Add(gtx.Ops)
		pad := dp(gtx, theme.Space2)
		inner := gtx
		inner.Constraints.Max.X = max(0, gtx.Constraints.Max.X-2*pad)
		call, dims := giodraw.Record(inner, spec.content)
		size := image.Pt(gtx.Constraints.Max.X, max(dims.Size.Y+2*pad, dp(gtx, theme.TargetMin)))
		radius := dp(gtx, theme.RadiusEdit)
		if spec.selected {
			giodraw.FillRRect(gtx, p.Tone, image.Rectangle{Max: size}, radius)
			giodraw.FillRRect(gtx, p.Text, image.Rect(0, 0, dp(gtx, theme.SelectionBarWidth), size.Y), 0)
		}
		giodraw.Place(gtx, image.Pt(pad, pad), call)
		n := platform.SemanticNode{ID: platform.NodeID(access.Path("option", spec.label)), Role: platform.RoleButton, Name: spec.label, Selectable: true, Selected: spec.selected, Action: "select"}
		access.Add(gtx, image.Rectangle{Max: size}, n, access.Actions{Click: spec.click})
		if gtx.Focused(spec.click) {
			focusRing(gtx, p.Text, size, radius)
		}
		return layout.Dimensions{Size: size}
	})
}
