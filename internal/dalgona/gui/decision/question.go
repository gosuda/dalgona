//go:build gui

package decision

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"image"
	"slices"
	"strings"
	"unicode/utf8"

	giofont "gioui.org/font"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type answer struct {
	picks    []widget.Clickable
	picked   []bool
	field    *platform.Editor
	copy     widget.Clickable
	fault    string
	rejected string
}

func (c *card) prepareAnswers() {
	if c.details.kind != kindQuestion {
		c.asked, c.answers = nil, nil
		return
	}
	asked := c.details.questions.Questions
	if slices.EqualFunc(c.asked, asked, sameQuestion) && len(c.answers) == len(asked) {
		c.asked = asked
		return
	}
	c.asked = asked
	c.answers = make([]answer, len(asked))
	for i, q := range asked {
		c.answers[i].prepare(q)
	}
}

func (a *answer) prepare(q rpc.ToolRequestUserInputQuestion) {
	a.picks = make([]widget.Clickable, len(q.Options))
	a.picked = make([]bool, len(q.Options))
	field, err := platform.NewEditor(textinput.Answer, textinput.Draft{})
	if err != nil {
		a.fault = err.Error()
		return
	}
	if q.IsSecret {
		field.Mask = secretMask
	}
	a.field = field
}

const secretMask = '•'

func sameQuestion(a, b rpc.ToolRequestUserInputQuestion) bool {
	return a.ID == b.ID && a.Header == b.Header && a.Question == b.Question && a.IsOther == b.IsOther &&
		a.IsSecret == b.IsSecret && a.MultiSelect == b.MultiSelect && slices.Equal(a.Options, b.Options)
}

func (c *card) updateAnswers(gtx layout.Context, env shell.Env, p pending) {
	c.triggers = c.triggers[:0]
	editable := c.editable(p)
	for i := range c.answers {
		c.answers[i].connect(env)
		c.triggers = c.answers[i].update(gtx, c.asked[i], editable, c.triggers)
	}
}

func (a *answer) connect(env shell.Env) {
	if a.field != nil {
		a.field.Clipboard, a.field.Wake = env.Clipboard, env.Wake
	}
}

func (a *answer) update(gtx layout.Context, q rpc.ToolRequestUserInputQuestion, editable bool, triggers []platform.EditorEventKind) []platform.EditorEventKind {
	if a.copy.Clicked(gtx) {
		a.copyRejected(gtx)
	}
	for j := range a.picks {
		if a.picks[j].Clicked(gtx) && editable {
			a.choose(q, j)
		}
	}
	if !editable {
		return triggers
	}
	for a.field != nil {
		ev, ok := a.field.Update(gtx)
		if !ok {
			break
		}
		triggers = a.apply(q, ev, triggers)
	}
	return triggers
}

func (a *answer) apply(q rpc.ToolRequestUserInputQuestion, ev platform.EditorEvent, triggers []platform.EditorEventKind) []platform.EditorEventKind {
	switch ev.Kind {
	case platform.EditorChange:
		a.fault, a.rejected = "", ""
		if !q.MultiSelect && a.text() != "" {
			clear(a.picked)
		}
	case platform.EditorRejected:
		a.rejected = a.field.TakeRejected()
		a.fault = rejectionText(ev.Err, a.rejected)
	case platform.EditorSubmit, platform.EditorCancel:
		triggers = append(triggers, ev.Kind)
	case platform.EditorKey:
	}
	return triggers
}

func (a *answer) text() string {
	if a.field == nil {
		return ""
	}
	return a.field.Draft().Text
}

func (a *answer) drafted() bool {
	return slices.Contains(a.picked, true) || a.text() != "" || a.field != nil && a.field.Composing()
}

func (a *answer) choose(q rpc.ToolRequestUserInputQuestion, option int) {
	if q.MultiSelect {
		a.picked[option] = !a.picked[option]
		return
	}
	if a.text() != "" {
		if err := a.field.SetDraft(textinput.Draft{}); err != nil {
			a.fault = rejectionText(err, "")
			return
		}
	}
	clear(a.picked)
	a.picked[option] = true
}

func (a *answer) copyRejected(gtx layout.Context) {
	if err := platform.WriteText(gtx.Source, a.rejected); err != nil {
		a.fault = err.Error()
	}
}

func rejectionText(err error, candidate string) string {
	if errors.Is(err, platform.ErrComposing) {
		return "Finish or cancel the text composition first."
	}
	reason, ok := errors.AsType[textinput.Reason](err)
	if !ok {
		return err.Error()
	}
	return faultText(reason, utf8.RuneCountInString(candidate))
}

func (c *card) submission() jsontext.Value {
	out := make(map[string]rpc.ToolRequestUserInputAnswer, len(c.asked))
	for i, q := range c.asked {
		a := &c.answers[i]
		list := make([]string, 0, len(q.Options)+1)
		for j, o := range q.Options {
			if a.picked[j] {
				list = append(list, o.Label)
			}
		}
		if typed := a.text(); typed != "" {
			list = append(list, typed)
		}
		out[q.ID] = rpc.ToolRequestUserInputAnswer{Answers: list}
	}
	return answersResult(out)
}

func (c *card) skipping() jsontext.Value {
	out := make(map[string]rpc.ToolRequestUserInputAnswer, len(c.asked))
	for _, q := range c.asked {
		out[q.ID] = rpc.ToolRequestUserInputAnswer{Answers: []string{}}
	}
	return answersResult(out)
}

func correctionTarget(field string) string {
	rest, ok := strings.CutPrefix(field, "answers.")
	at := strings.LastIndex(rest, ".answers[")
	if !ok || at <= 0 || !strings.HasSuffix(rest, "]") {
		return ""
	}
	return rest[:at]
}

func fieldShown(q rpc.ToolRequestUserInputQuestion) bool {
	return q.IsOther || len(q.Options) == 0
}

func (a *answer) problem(q rpc.ToolRequestUserInputQuestion, w *warningNote) string {
	if a.fault != "" {
		return a.fault
	}
	if w != nil && w.question == q.ID {
		return correctionText(w.reason)
	}
	return ""
}

func preview(q rpc.ToolRequestUserInputQuestion, a *answer) string {
	for j := len(q.Options) - 1; j >= 0; j-- {
		if a.picked[j] && q.Options[j].Preview != "" {
			return q.Options[j].Preview
		}
	}
	return ""
}

type warningNote struct {
	question string
	reason   string
	message  string
}

func (d *Dock) questionItem(ps paints, f frame, index int) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		c := f.card
		q, a := c.asked[index], &c.answers[index]
		editable := c.editable(f.pending)
		problem := a.problem(q, f.warning)
		top := theme.Space4
		if index == 0 {
			top = 0
		}
		return access.Scope(gtx, access.Path("question", q.ID), func(gtx layout.Context) layout.Dimensions {
			return d.questionBody(gtx, ps, q, a, questionLayout{top: top, editable: editable, problem: problem})
		})
	}
}

type questionLayout struct {
	top      theme.Dp
	editable bool
	problem  string
}

func (d *Dock) questionBody(gtx layout.Context, ps paints, q rpc.ToolRequestUserInputQuestion, a *answer, opts questionLayout) layout.Dimensions {
	return layout.Inset{Top: unit.Dp(opts.top)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return column(gtx, theme.Space2,
			func(gtx layout.Context) layout.Dimensions {
				return drawText(gtx, ps.shaper, label(ps.palette.Dim), q.Header)
			},
			func(gtx layout.Context) layout.Dimensions {
				return drawText(gtx, ps.shaper, prose(ps.palette.Text), q.Question)
			},
			d.optionList(ps, q, a, opts.editable),
			d.answerField(ps, q, a, opts.editable, opts.problem),
			d.problemLine(ps, a, q.IsSecret, opts.problem),
			d.previewBlock(ps, preview(q, a)),
		)
	})
}

type choiceRow struct {
	option   rpc.ToolRequestUserInputOption
	click    *widget.Clickable
	role     platform.Role
	first    bool
	picked   bool
	editable bool
}

func (d *Dock) optionList(ps paints, q rpc.ToolRequestUserInputQuestion, a *answer, editable bool) layout.Widget {
	if len(q.Options) == 0 {
		return nil
	}
	role := platform.RoleRadioButton
	if q.MultiSelect {
		role = platform.RoleCheckBox
	}
	return func(gtx layout.Context) layout.Dimensions {
		rows := make([]layout.Widget, len(q.Options))
		for j, o := range q.Options {
			row := choiceRow{option: o, click: &a.picks[j], role: role, first: j == 0, picked: a.picked[j], editable: editable}
			rows[j] = func(gtx layout.Context) layout.Dimensions { return d.option(gtx, ps, row) }
		}
		return column(gtx, theme.Space2, rows...)
	}
}

func (d *Dock) option(gtx layout.Context, ps paints, row choiceRow) layout.Dimensions {
	if !row.editable {
		gtx = gtx.Disabled()
	}
	about := row.option.Description
	if row.first {
		about = "Recommended. " + about
	}
	return row.click.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		n := platform.SemanticNode{ID: platform.NodeID(access.Path("option", row.option.Label)), Role: row.role, Name: row.option.Label, Description: about, Checked: row.picked, Disabled: !row.editable, Action: "select"}
		n.Add(gtx.Ops)
		dims := d.optionFace(gtx, ps, row)
		access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{Click: row.click})
		return dims
	})
}

func (d *Dock) optionFace(gtx layout.Context, ps paints, row choiceRow) layout.Dimensions {
	p := ps.palette
	padB, padI := dp(gtx, theme.OptionPaddingBlock), dp(gtx, theme.OptionPaddingInline)
	markColumn, gap := dp(gtx, theme.OptionMarkColumn), dp(gtx, theme.OptionGap)
	width := gtx.Constraints.Max.X
	inner := sized(gtx)
	inner.Constraints.Max.X = max(0, width-2*padI-markColumn-gap)
	textCall, textDims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		return column(gtx, 0,
			func(gtx layout.Context) layout.Dimensions { return d.choiceLine(gtx, ps, row.option.Label, row.first) },
			func(gtx layout.Context) layout.Dimensions {
				return drawText(gtx, ps.shaper, small(p.Dim), row.option.Description)
			})
	})
	size := image.Pt(width, max(dp(gtx, theme.OptionHeight), textDims.Size.Y+2*padB))
	radius := dp(gtx, theme.RadiusTag)
	bounds := image.Rectangle{Max: size}
	if row.picked {
		giodraw.FillRRect(gtx, p.Tone, bounds, radius)
		giodraw.FillRect(gtx, p.Text, image.Rect(0, radius/2, dp(gtx, theme.SelectionBarWidth), size.Y-radius/2))
	}
	giodraw.StrokeRRect(gtx, p.Line, bounds, radius, dp(gtx, theme.BorderWidth))
	optionMark(gtx, p, image.Pt(padI+(markColumn-dp(gtx, theme.OptionMarkSize))/2, padB+dp(gtx, theme.Space1)), row)
	giodraw.Place(gtx, image.Pt(padI+markColumn+gap, padB), textCall)
	if gtx.Focused(row.click) {
		focusRing(gtx, p.Text, size, radius)
	}
	return layout.Dimensions{Size: size}
}

func (d *Dock) choiceLine(gtx layout.Context, ps paints, choice string, recommended bool) layout.Dimensions {
	st := textStyle{face: platform.ProseFace, weight: theme.WeightTitle, size: theme.SizeLabel, leading: theme.LeadingOption, color: ps.palette.Text}
	if !recommended {
		return drawText(gtx, ps.shaper, st, choice)
	}
	return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Baseline}.Layout(gtx,
		layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
			gtx.Constraints.Min.X = 0
			return drawText(gtx, ps.shaper, st, choice)
		}),
		layout.Rigid(layout.Spacer{Width: unit.Dp(theme.Space2)}.Layout),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, label(ps.palette.Dim), "Recommended")
		}),
	)
}

func optionMark(gtx layout.Context, p theme.Palette, at image.Point, row choiceRow) {
	size := dp(gtx, theme.OptionMarkSize)
	box := image.Rectangle{Min: at, Max: at.Add(image.Pt(size, size))}
	radius := size / 2
	if row.role == platform.RoleCheckBox {
		radius = dp(gtx, theme.RadiusEdit)
	}
	giodraw.StrokeRRect(gtx, p.Strong, box, radius, dp(gtx, theme.BorderWidth))
	if !row.picked {
		return
	}
	dot := dp(gtx, theme.Space2)
	inner := image.Rectangle{Min: at.Add(image.Pt((size-dot)/2, (size-dot)/2))}
	inner.Max = inner.Min.Add(image.Pt(dot, dot))
	giodraw.FillRRect(gtx, p.Text, inner, min(radius, dot/2))
}

type fieldLook struct {
	hint, name string
	role       platform.Role
}

func fieldLookOf(q rpc.ToolRequestUserInputQuestion) fieldLook {
	look := fieldLook{hint: "Answer", name: "Answer for " + q.Header, role: platform.RoleTextField}
	if len(q.Options) > 0 {
		look.hint, look.name = "Other answer", "Other answer for "+q.Header
	}
	if q.IsSecret {
		look.role = platform.RoleSecureField
	}
	return look
}

func (d *Dock) answerField(ps paints, q rpc.ToolRequestUserInputQuestion, a *answer, editable bool, problem string) layout.Widget {
	if !fieldShown(q) || a.field == nil {
		return nil
	}
	look := fieldLookOf(q)
	return func(gtx layout.Context) layout.Dimensions {
		p := ps.palette
		padI, padB := dp(gtx, theme.OptionPaddingInline), dp(gtx, theme.Space2)
		width := gtx.Constraints.Max.X
		inner := sized(gtx)
		inner.Constraints.Max.X = max(0, width-2*padI)
		inner.Constraints.Min.X = inner.Constraints.Max.X
		if !editable {
			inner = inner.Disabled()
		}
		call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
			return a.field.Layout(gtx, platform.EditorStyle{
				Shaper: ps.shaper, Font: giofont.Font{Typeface: platform.ProseFace, Weight: platform.FontWeight(theme.WeightText)},
				Size: unit.Sp(theme.InputTextSize), Text: giodraw.Material(gtx, p.Text), Selection: giodraw.Material(gtx, selectionTint(p)),
			})
		})
		size := image.Pt(width, max(dp(gtx, theme.FieldActionHeight), dims.Size.Y+2*padB))
		radius := dp(gtx, theme.RadiusTag)
		edge := p.Strong
		if problem != "" {
			edge = p.FailedText
		}
		giodraw.StrokeRRect(gtx, edge, image.Rectangle{Max: size}, radius, dp(gtx, theme.BorderWidth))
		if a.text() == "" && !a.field.Composing() {
			d.fieldHint(gtx, ps, inner, image.Pt(padI, padB), look.hint)
		}
		area := clip.Rect{Min: image.Pt(padI, padB), Max: image.Pt(padI+dims.Size.X, padB+dims.Size.Y)}.Push(gtx.Ops)
		n := platform.SemanticNode{ID: "answer", Role: look.role, Name: look.name, Description: problem, Placeholder: look.hint, Disabled: !editable, Action: "edit"}
		n.Add(gtx.Ops)
		area.Pop()
		giodraw.Place(gtx, image.Pt(padI, padB), call)
		if gtx.Focused(a.field.Tag()) {
			focusRing(gtx, p.Text, size, radius)
		}
		a.registerField(gtx, size, n)
		return layout.Dimensions{Size: size}
	}
}

func (a *answer) registerField(gtx layout.Context, size image.Point, n platform.SemanticNode) {
	access.Add(gtx, image.Rectangle{Max: size}, n, access.Actions{Editor: a.field, Rejected: func(err error) {
		a.rejected = a.field.TakeRejected()
		a.fault = rejectionText(err, a.rejected)
	}})
}

func (d *Dock) fieldHint(gtx layout.Context, ps paints, inner layout.Context, at image.Point, hint string) {
	call, _ := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
		return drawText(gtx, ps.shaper, textStyle{face: platform.ProseFace, weight: theme.WeightText, size: theme.InputTextSize, leading: theme.LeadingBody, color: ps.palette.Dim, maxLines: 1}, hint)
	})
	giodraw.Place(gtx, at, call)
}

func (d *Dock) problemLine(ps paints, a *answer, masked bool, problem string) layout.Widget {
	if problem == "" {
		return nil
	}
	notice := announced(ps, small(ps.palette.FailedText), platform.RoleError, problem)
	if a.rejected == "" || masked {
		return notice
	}
	restore := button{click: &a.copy, label: "Copy rejected text", variant: variantSecondary, height: theme.ControlHeight, size: theme.SizeLabel}
	return func(gtx layout.Context) layout.Dimensions {
		return column(gtx, theme.Space2, notice, func(gtx layout.Context) layout.Dimensions { return restore.layout(sized(gtx), ps) })
	}
}

func (d *Dock) previewBlock(ps paints, content string) layout.Widget {
	if content == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		padB, padI := dp(gtx, theme.Space2), dp(gtx, theme.Space3)
		width := gtx.Constraints.Max.X
		inner := sized(gtx)
		inner.Constraints.Max.X = max(0, width-2*padI)
		call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, mono(ps.palette.Text, theme.SizeCaption), content)
		})
		size := image.Pt(width, dims.Size.Y+2*padB)
		giodraw.FillRRect(gtx, ps.palette.Tone, image.Rectangle{Max: size}, dp(gtx, theme.RadiusControl))
		giodraw.Place(gtx, image.Pt(padI, padB), call)
		return layout.Dimensions{Size: size}
	}
}

func faultText(reason textinput.Reason, scalars int) string {
	switch reason {
	case textinput.TextTooLarge:
		return fmt.Sprintf("This answer has %d characters. Shorten it to %d or fewer.", scalars, textinput.Answer.Limit.Max)
	case textinput.DisallowedControl:
		return "This text has a NUL character. Remove it to continue."
	case textinput.LineBreakNotAllowed:
		return "This field takes one line. Remove the line breaks to continue."
	case textinput.InvalidEncoding, textinput.InvalidOffset:
	}
	return "This text has an incomplete character. Retype the last character."
}

func correctionText(reason string) string {
	switch textinput.Reason(reason) {
	case textinput.TextTooLarge:
		return fmt.Sprintf("dalgona refused this answer because it has more than %d characters. Shorten it and submit again.", textinput.Answer.Limit.Max)
	case textinput.DisallowedControl:
		return "dalgona refused this answer because it has a NUL character. Remove it and submit again."
	case textinput.InvalidEncoding:
		return "dalgona refused this answer because it has an incomplete character. Retype it and submit again."
	case textinput.LineBreakNotAllowed:
		return "dalgona refused this answer because it has a line break. Remove it and submit again."
	case textinput.InvalidOffset:
	}
	return "dalgona refused this answer. Edit it and submit again."
}
