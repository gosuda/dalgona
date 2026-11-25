//go:build gui

package decision

import (
	"image"
	"strings"

	"gioui.org/layout"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func (d *Dock) detailItems(ps paints, f frame) []layout.Widget {
	c, p := f.card, f.pending
	switch {
	case p.view.Err != nil:
		return []layout.Widget{d.note(ps, "The request details are not shown because they did not load.")}
	case p.view.Params == nil:
		return []layout.Widget{d.note(ps, "Loading the exact request details from "+p.endpoint+".")}
	case c.details.malformed != nil:
		n := platform.SemanticNode{ID: "request-details", Name: "Request details", Value: string(p.view.Params), Role: platform.RoleCode}
		return []layout.Widget{codeText(ps, n, theme.SizeCaption)}
	}
	return d.kindItems(ps, f)
}

func (d *Dock) kindItems(ps paints, f frame) []layout.Widget {
	switch f.card.details.kind {
	case kindCommand:
		return d.commandItems(ps, f)
	case kindFile:
		return d.fileItems(ps, f)
	case kindTrust:
		return d.trustItems(ps, f)
	case kindPermission:
		return d.permissionItems(ps, f)
	case kindSandbox:
		return d.sandboxItems(ps, f)
	case kindQuestion:
	}
	return d.questionItems(ps, f)
}

func (d *Dock) childItem(ps paints, f frame) layout.Widget {
	if f.pending.child == "" {
		return nil
	}
	return d.note(ps, "From subagent "+f.pending.child)
}

func compact(items ...layout.Widget) []layout.Widget {
	out := items[:0]
	for _, w := range items {
		if w != nil {
			out = append(out, w)
		}
	}
	return out
}

func (d *Dock) commandItems(ps paints, f frame) []layout.Widget {
	params := f.card.details.command
	return compact(
		d.commandBlock(ps, params.Command, params.Cwd),
		d.escalation(ps, params.Escalated),
		d.reason(ps, params.Reason),
		d.childItem(ps, f),
		d.note(ps, commandGrantNote),
	)
}

func (d *Dock) escalation(ps paints, escalated bool) layout.Widget {
	if !escalated {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space3)}.Layout(gtx, announced(ps, small(ps.palette.WarningText), platform.RoleStatus, escalationNote))
	}
}

func (d *Dock) trustItems(ps paints, f frame) []layout.Widget {
	params := f.card.details.command
	command := platform.SemanticNode{ID: "command", Name: "Command", Value: params.Command, Role: platform.RoleCode}
	return compact(
		d.fact(ps, "Workspace", params.Cwd),
		d.fact(ps, "Servers", ""),
		codeText(ps, command, theme.SizeSmall),
		d.reason(ps, params.Reason),
		d.childItem(ps, f),
		d.note(ps, trustNote),
	)
}

func (d *Dock) permissionItems(ps paints, f frame) []layout.Widget {
	params := f.card.details.permission
	items := compact(
		d.fact(ps, "Thread", params.ThreadID),
		d.fact(ps, "Runner", params.RunnerID),
		d.fact(ps, "Binding", params.BindingRevision),
		d.fact(ps, "Root", params.Root),
		d.fact(ps, "Rights", strings.Join(params.Rights, ", ")),
		d.reason(ps, params.Reason),
		d.childItem(ps, f),
	)
	for _, note := range rightsNotes(params.Rights) {
		items = append(items, d.note(ps, note))
	}
	return append(items, d.note(ps, handleNote))
}

func (d *Dock) sandboxItems(ps paints, f frame) []layout.Widget {
	q := f.card.details.questions.Questions[0]
	return compact(
		d.reason(ps, q.Question),
		d.consequence(ps, q.Options[0].Label, q.Options[0].Description, sandboxAcceptNote),
		d.consequence(ps, q.Options[1].Label, q.Options[1].Description, sandboxDangerousNote),
		d.childItem(ps, f),
		d.note(ps, sandboxHoldNote),
	)
}

func (d *Dock) questionItems(ps paints, f frame) []layout.Widget {
	items := make([]layout.Widget, 0, len(f.card.asked)+3)
	if f.card.locked != "" {
		items = append(items, announced(ps, small(ps.palette.WarningText), platform.RoleStatus, f.card.locked))
	}
	if f.warning != nil && !f.card.askedID(f.warning.question) {
		items = append(items, announced(ps, small(ps.palette.FailedText), platform.RoleError, f.warning.message))
	}
	if w := d.childItem(ps, f); w != nil {
		items = append(items, w)
	}
	for i := range f.card.asked {
		items = append(items, d.questionItem(ps, f, i))
	}
	return items
}

func (c *card) askedID(id string) bool {
	for _, q := range c.asked {
		if q.ID == id {
			return true
		}
	}
	return false
}

func (d *Dock) commandBlock(ps paints, command, cwd string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		bar, padB, padI := dp(gtx, theme.Space1), dp(gtx, theme.CommandPaddingBlock), dp(gtx, theme.Space5)
		width := gtx.Constraints.Max.X
		inner := sized(gtx)
		inner.Constraints.Max.X = max(0, width-bar-2*padI)
		call, dims := giodraw.Record(inner, func(gtx layout.Context) layout.Dimensions {
			return column(gtx, theme.DirectoryMargin,
				func(gtx layout.Context) layout.Dimensions {
					st := mono(ps.palette.Text, theme.SizeDisplay)
					st.weight, st.leading = theme.WeightTitle, theme.LeadingCommand
					return semanticText(gtx, ps, st, platform.SemanticNode{ID: "command", Name: "Command", Value: command, Role: platform.RoleCode})
				},
				d.directory(ps, cwd),
			)
		})
		size := image.Pt(width, max(dp(gtx, theme.CommandMinHeight), dims.Size.Y+2*padB))
		giodraw.FillRect(gtx, ps.palette.Tone, image.Rectangle{Max: size})
		giodraw.FillRect(gtx, ps.palette.Text, image.Rect(0, 0, bar, size.Y))
		giodraw.Place(gtx, image.Pt(bar+padI, (size.Y-dims.Size.Y)/2), call)
		return layout.Dimensions{Size: size}
	}
}

func (d *Dock) directory(ps paints, cwd string) layout.Widget {
	if cwd == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		st := mono(ps.palette.Dim, theme.SizeSmall)
		st.leading = theme.LeadingDirectory
		return semanticText(gtx, ps, st, platform.SemanticNode{ID: "workspace", Name: "Workspace", Value: cwd, Role: platform.RoleDocument})
	}
}

func (d *Dock) reason(ps paints, text string) layout.Widget {
	if text == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space3)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return semanticText(gtx, ps, prose(ps.palette.Text), platform.SemanticNode{ID: "reason", Name: "Reason", Value: text, Role: platform.RoleDocument})
		})
	}
}

func semanticText(gtx layout.Context, ps paints, st textStyle, n platform.SemanticNode) layout.Dimensions {
	dims := drawText(gtx, ps.shaper, st, n.Value)
	access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{})
	return dims
}

func (d *Dock) note(ps paints, text string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space3)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, small(ps.palette.Dim), text)
		})
	}
}

func codeText(ps paints, n platform.SemanticNode, size theme.Sp) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return semanticText(gtx, ps, mono(ps.palette.Text, size), n)
		})
	}
}

func (d *Dock) fact(ps paints, name, value string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			dims := column(gtx, 0,
				func(gtx layout.Context) layout.Dimensions {
					return drawText(gtx, ps.shaper, label(ps.palette.Dim), name)
				},
				d.factValue(ps, value),
			)
			n := platform.SemanticNode{ID: platform.NodeID(access.Path("fact", name)), Role: platform.RoleDocument, Name: name, Value: value}
			access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{})
			return dims
		})
	}
}

func (d *Dock) factValue(ps paints, value string) layout.Widget {
	if value == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		return drawText(gtx, ps.shaper, mono(ps.palette.Text, theme.SizeSmall), value)
	}
}

func (d *Dock) consequence(ps paints, choice, description, effect string) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space3)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return column(gtx, theme.Space1,
				func(gtx layout.Context) layout.Dimensions {
					st := prose(ps.palette.Text)
					st.weight = theme.WeightTitle
					return drawText(gtx, ps.shaper, st, choice)
				},
				func(gtx layout.Context) layout.Dimensions {
					return drawText(gtx, ps.shaper, small(ps.palette.Dim), description)
				},
				func(gtx layout.Context) layout.Dimensions {
					return drawText(gtx, ps.shaper, small(ps.palette.Text), effect)
				},
			)
		})
	}
}
