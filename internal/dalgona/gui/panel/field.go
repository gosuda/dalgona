//go:build gui

package panel

import (
	"errors"
	"image"
	"image/color"
	"strconv"

	giofont "gioui.org/font"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const secretMask = '•'

var faultNotices = map[textinput.Reason]string{
	textinput.InvalidEncoding:     "This text has an incomplete character. Retype the last character.",
	textinput.DisallowedControl:   "This text has a NUL character. Remove it to continue.",
	textinput.LineBreakNotAllowed: "This field takes one line. Paste or drop text without line breaks.",
}

func faultNotice(err error, policy textinput.Policy) string {
	var reason textinput.Reason
	if !errors.As(err, &reason) {
		return err.Error()
	}
	if reason == textinput.TextTooLarge {
		return "This text is over the " + strconv.Itoa(policy.Limit.Max) + " " + unitName(policy.Limit.Unit) + " limit. Shorten it to continue."
	}
	if notice, ok := faultNotices[reason]; ok {
		return notice
	}
	return string(reason)
}

func unitName(u textinput.Unit) string {
	if u == textinput.Bytes {
		return "byte"
	}
	return "character"
}

type field struct {
	editor *platform.Editor
	label  string
	policy textinput.Policy
	secret bool
	fault  string
}

type fieldEvents struct {
	submit, cancel, changed bool
}

func newField(label string, policy textinput.Policy) field {
	return field{label: label, policy: policy}
}

func (f *field) ensure() *platform.Editor {
	if f.editor != nil {
		return f.editor
	}
	editor, err := platform.NewEditor(f.policy, textinput.Draft{})
	if err != nil {
		f.fault = err.Error()
		return nil
	}
	if f.secret {
		editor.Mask = secretMask
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

func (f *field) clear() {
	if f.editor == nil {
		return
	}
	if err := f.editor.SetDraft(textinput.Draft{}); err != nil {
		f.editor = nil
	}
	f.fault = ""
}

func (f *field) update(gtx layout.Context) fieldEvents {
	var out fieldEvents
	e := f.ensure()
	if e == nil {
		return out
	}
	for {
		ev, ok := e.Update(gtx)
		if !ok {
			return out
		}
		switch ev.Kind {
		case platform.EditorChange:
			f.fault, out.changed = "", true
		case platform.EditorRejected:
			e.TakeRejected()
			f.fault = faultNotice(ev.Err, f.policy)
		case platform.EditorSubmit:
			out.submit = true
		case platform.EditorCancel:
			out.cancel = true
		case platform.EditorKey:
		}
	}
}

func (f *field) layout(gtx layout.Context, s *scene) layout.Dimensions {
	e := f.ensure()
	p := s.env.Theme.Palette()
	children := []layout.FlexChild{layout.Rigid(s.legend(f.label))}
	if e != nil {
		e.Clipboard, e.Wake = s.env.Clipboard, s.env.Wake
		s.frame.Focusable(e.Tag())
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions { return f.box(gtx, s, e) }))
	}
	if f.fault != "" {
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			dims := drawText(gtx, s.env.Shaper, failureText(p), f.fault)
			if e != nil {
				id := access.Path("error", f.label)
				access.Add(gtx, image.Rectangle{Max: dims.Size}, platform.SemanticNode{ID: platform.NodeID(id), Role: platform.RoleError, Name: f.fault, Target: access.ID(gtx, f.semanticID())}, access.Actions{})
			}
			return dims
		}))
	}
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func (f *field) box(gtx layout.Context, s *scene, e *platform.Editor) layout.Dimensions {
	p := s.env.Theme.Palette()
	padX, padY := dp(gtx, theme.Space3), dp(gtx, theme.Space2)
	inner := gtx
	inner.Constraints = layout.Constraints{Max: image.Pt(max(0, gtx.Constraints.Max.X-2*padX), gtx.Constraints.Max.Y)}
	inner.Constraints.Min.X = inner.Constraints.Max.X
	call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		semantic.LabelOp(f.label).Add(gtx.Ops)
		return e.Layout(gtx, platform.EditorStyle{
			Shaper: s.env.Shaper, Font: giofont.Font{Typeface: platform.ProseFace, Weight: platform.FontWeight(theme.WeightText)},
			Size: unit.Sp(theme.SizeBody), Text: giodraw.Material(gtx, p.Text), Selection: giodraw.Material(gtx, selectionTint(p)),
		})
	})
	size := image.Pt(gtx.Constraints.Max.X, max(dims.Size.Y+2*padY, dp(gtx, theme.ControlHeight)))
	radius := dp(gtx, theme.RadiusControl)
	box := image.Rectangle{Max: size}
	giodraw.FillRRect(gtx, p.Canvas, box, radius)
	edge := p.Strong
	if f.fault != "" {
		edge = p.FailedText
	}
	giodraw.StrokeRRect(gtx, edge, box, radius, dp(gtx, theme.BorderWidth))
	giodraw.Place(gtx, image.Pt(padX, (size.Y-dims.Size.Y)/2), call)
	if gtx.Focused(e.Tag()) {
		focusRing(gtx, p.Text, size, radius)
	}
	f.recordField(gtx, size, e)
	return layout.Dimensions{Size: size}
}

func (f *field) semanticID() string { return access.Path("field", f.label) }

func (f *field) recordField(gtx layout.Context, size image.Point, e *platform.Editor) {
	role := platform.RoleTextField
	if f.secret {
		role = platform.RoleSecureField
	}
	n := platform.SemanticNode{ID: platform.NodeID(f.semanticID()), Role: role, Name: f.label, Description: f.fault, Action: "edit"}
	access.Add(gtx, image.Rectangle{Max: size}, n, access.Actions{Editor: e, Rejected: func(err error) { f.fault = faultNotice(err, f.policy) }})
}

func selectionTint(p theme.Palette) color.NRGBA {
	c := p.Strong
	c.A = 0x66
	return c
}
