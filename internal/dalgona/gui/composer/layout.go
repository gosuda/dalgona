//go:build gui

package composer

import (
	"image"

	giofont "gioui.org/font"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/semantic"
	"gioui.org/layout"
	"gioui.org/op/clip"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	labelMessage = "Message"
	labelShell   = "Shell command"
	labelFiles   = "Files"
	labelCommand = "Commands"
	helpIdle     = "Enter to send · Shift+Enter for a new line"
	helpRunning  = "Enter to steer · Shift+Enter for a new line · Alt+Enter to queue"
	helpArmed    = "Esc again to interrupt"

	fieldGrowth      = 4
	previewLineLimit = 40
	previewByteLimit = 16 << 10
)

type dockWidgets struct {
	primary      widget.Clickable
	queue        widget.Clickable
	interrupt    widget.Clickable
	attach       widget.Clickable
	correct      widget.Clickable
	copyRejected widget.Clickable
}

func focusEditor(d *Dock) key.FocusCmd { return key.FocusCmd{Tag: d.editor.Tag()} }

func (d *Dock) handleClicks(gtx layout.Context, env frame) {
	d.queue.update(gtx, d, env)
	d.attach.rows.update(gtx, d, env)
	d.shell.update(gtx, d, env)
	if i, ok := d.palette.rows.clicked(gtx); ok {
		d.palette.active = i
		d.openSelected(env)
	}
	if i, ok := d.files.rows.clicked(gtx); ok {
		d.files.active = i
		d.files.insert(d)
		gtx.Execute(focusEditor(d))
	}
	d.actionClicks(gtx, env)
}

func (d *Dock) actionClicks(gtx layout.Context, env frame) {
	if d.ui.primary.Clicked(gtx) {
		d.act(env, false)
		gtx.Execute(focusEditor(d))
	}
	if d.ui.queue.Clicked(gtx) {
		d.act(env, true)
		gtx.Execute(focusEditor(d))
	}
	if d.ui.interrupt.Clicked(gtx) && running(env) {
		d.interrupt(env)
	}
	if d.ui.attach.Clicked(gtx) {
		d.pickImages(env)
	}
	if d.ui.correct.Clicked(gtx) {
		env.Open(shell.Route{Panel: "model", Thread: env.Thread})
	}
	if d.ui.copyRejected.Clicked(gtx) {
		d.copyRejected(gtx)
	}
}

func (d *Dock) copyRejected(gtx layout.Context) {
	if d.notice.rejected == "" {
		return
	}
	if err := platform.WriteText(gtx.Source, d.notice.rejected); err != nil {
		d.notice.fail(err.Error())
	}
}

func (d *Dock) pickImages(env frame) {
	if env.Thread != (state.ThreadKey{}) {
		if accepts, known := env.view.AcceptsImages(env.Thread); known && !accepts {
			d.notice.fail(string(errImagesRefused))
			return
		}
	}
	d.attach.pick(d)
}

func (d *Dock) layout(gtx layout.Context, env frame) layout.Dimensions {
	look := dockLook{shaper: env.Shaper, palette: env.Theme.Palette(), point: env.Theme.Point(env.Thread.Thread)}
	parts := []layout.Widget{
		d.part(len(env.view.FollowUps(d.key)) > 0, func(gtx layout.Context) layout.Dimensions { return d.queue.layout(gtx, d, env, look) }),
		d.part(len(env.view.ShellRuns(d.key)) > 0, func(gtx layout.Context) layout.Dimensions { return d.shell.layout(gtx, d, env, look) }),
		func(gtx layout.Context) layout.Dimensions { return d.field(gtx, env, look) },
		d.listPart(env, look),
		d.part(len(d.draft.chips) > 0, func(gtx layout.Context) layout.Dimensions { return d.attach.rows.layout(gtx, d, env, look) }),
		d.textPart(env, d.keywordLine(env), caption(look.palette.Dim)),
		d.textPart(env, d.help(gtx, env), caption(look.palette.Dim)),
		d.noticePart(env, look),
		func(gtx layout.Context) layout.Dimensions { return d.actions(gtx, env, look) },
	}
	path := access.Path("composer", string(env.Thread.Endpoint), string(env.Thread.Thread))
	n := platform.SemanticNode{ID: platform.NodeID(path), Role: platform.RoleDocument, Name: "Composer"}
	return access.Group(gtx, n, access.Actions{}, func(gtx layout.Context) layout.Dimensions { return column(gtx, theme.Space2, parts...) })
}

func (d *Dock) part(shown bool, w layout.Widget) layout.Widget {
	if !shown {
		return nil
	}
	return w
}

func (d *Dock) textPart(env frame, txt string, st textStyle) layout.Widget {
	if txt == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions { return drawText(gtx, env.Shaper, st, txt) }
}

func (d *Dock) entry() entry { return parseEntry(d.editor.Draft().Text) }

func (d *Dock) label() string {
	if d.entry().kind == entryShell {
		return labelShell
	}
	return labelMessage
}

func (d *Dock) help(gtx layout.Context, env frame) string {
	switch {
	case d.guard.armed(gtx.Now) && running(env):
		return helpArmed
	case d.entry().kind == entryShell:
		return "Enter runs the command in " + d.shellRoot(env) + ". It is not sent to the agent."
	case running(env):
		return helpRunning
	}
	return helpIdle
}

func (d *Dock) shellRoot(env frame) string {
	runs := env.view.ShellRuns(d.key)
	for i := len(runs) - 1; i >= 0; i-- {
		if runs[i].Root != "" {
			return runs[i].Root
		}
	}
	if fs := env.view.FileSearch(d.key); fs.Root != "" {
		return fs.Root
	}
	if tv, ok := threadOf(env); ok && tv.Thread.Cwd != "" {
		return tv.Thread.Cwd
	}
	return "the thread's runner workspace"
}

func (d *Dock) listPart(env frame, look dockLook) layout.Widget {
	switch {
	case d.palette.open(d):
		return func(gtx layout.Context) layout.Dimensions { return d.paletteView(gtx, env, look) }
	case d.files.requested():
		return func(gtx layout.Context) layout.Dimensions { return d.filesView(gtx, env, look) }
	}
	return nil
}

func (d *Dock) paletteView(gtx layout.Context, env frame, look dockLook) layout.Dimensions {
	rows := d.paletteRows(env)
	if len(rows) == 0 {
		query, _ := d.palette.current(d)
		return drawText(gtx, env.Shaper, caption(look.palette.Dim), unknownCommand(query))
	}
	return d.palette.rows.layout(gtx, look, labelCommand, rows, min(d.palette.active, len(rows)-1))
}

func (d *Dock) filesView(gtx layout.Context, env frame, look dockLook) layout.Dimensions {
	status := d.files.status(env)
	list := func(gtx layout.Context) layout.Dimensions {
		return d.files.rows.layout(gtx, look, labelFiles, fileRows(d.files.shown), min(d.files.active, len(d.files.shown)-1))
	}
	note := func(gtx layout.Context) layout.Dimensions {
		return drawText(gtx, env.Shaper, caption(look.palette.Dim), status)
	}
	switch {
	case len(d.files.shown) == 0:
		return note(gtx)
	case status == "":
		return list(gtx)
	}
	return column(gtx, theme.Space1, list, note)
}

func (d *Dock) noticeText(env frame) string {
	if d.notice.text == "" && imagesRefused(env, d.draft.chips) {
		return string(errImagesRefused)
	}
	return d.notice.text
}

func (d *Dock) noticePart(env frame, look dockLook) layout.Widget {
	message := d.noticeText(env)
	if message == "" {
		return nil
	}
	return func(gtx layout.Context) layout.Dimensions {
		text := func(gtx layout.Context) layout.Dimensions {
			call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
				return drawText(gtx, env.Shaper, small(look.palette.FailedText), message)
			})
			defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
			n := platform.SemanticNode{ID: "validation", Role: platform.RoleError, Name: message, Target: access.ID(gtx, "message")}
			n.Add(gtx.Ops)
			access.Add(gtx, image.Rectangle{Max: dims.Size}, n, access.Actions{})
			call.Add(gtx.Ops)
			return dims
		}
		if d.notice.rejected == "" {
			return text(gtx)
		}
		copyText := control{click: &d.ui.copyRejected, label: "Copy rejected text", variant: variantSecondary}
		return flow(gtx, theme.Space3, text, func(gtx layout.Context) layout.Dimensions {
			return copyText.layout(gtx, env.Shaper, look.palette, look.point)
		})
	}
}

func (d *Dock) primaryControl(env frame) control {
	c := control{click: &d.ui.primary, label: "Send", variant: variantPoint, height: theme.FieldActionHeight}
	switch {
	case d.entry().kind == entryShell:
		c.label, c.variant = "Run", variantSecondary
		c.reason = d.refuseShellReason(env)
	case running(env):
		c.label, c.variant = "Steer", variantSecondary
		c.reason = sendBlock(env)
	default:
		c.reason = sendBlock(env)
	}
	if c.variant == variantPoint && (env.Deciding || env.Modal) {
		c.variant = variantSecondary
	}
	return c
}

func (d *Dock) refuseShellReason(env frame) string {
	if tv, ok := threadOf(env); !ok || tv.Thread.Cwd == "" {
		return noticeShellNone
	}
	return ""
}

func (d *Dock) field(gtx layout.Context, env frame, look dockLook) layout.Dimensions {
	label := func(gtx layout.Context) layout.Dimensions {
		st := textStyle{face: platform.TitleFace, weight: theme.WeightControl, size: theme.SizeSmall, leading: theme.LeadingTight, color: look.palette.Text}
		return drawText(gtx, env.Shaper, st, d.label())
	}
	box := func(gtx layout.Context) layout.Dimensions { return d.fieldBox(gtx, env, look) }
	return column(gtx, theme.Space2, label, box)
}

func (d *Dock) fieldBox(gtx layout.Context, env frame, look dockLook) layout.Dimensions {
	primary := d.primaryControl(env)
	inset := layout.Inset{Top: unit.Dp(theme.Space2), Right: unit.Dp(theme.Space2), Bottom: unit.Dp(theme.Space2), Left: unit.Dp(theme.Space5)}
	gtx.Constraints.Min = image.Pt(gtx.Constraints.Max.X, dp(gtx, theme.FieldHeight))
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return inset.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Middle, Spacing: layout.SpaceBetween}.Layout(gtx,
				layout.Flexed(1, func(gtx layout.Context) layout.Dimensions { return d.editorView(gtx, env, look) }),
				layout.Rigid(layout.Spacer{Width: unit.Dp(theme.Space3)}.Layout),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions {
					return primary.layout(gtx, env.Shaper, look.palette, look.point)
				}),
			)
		})
	})
	bounds := image.Rectangle{Max: dims.Size}
	radius := dp(gtx, theme.RadiusField)
	giodraw.FillRRect(gtx, look.palette.Surface, bounds, radius)
	edge := look.palette.Strong
	if d.noticeText(env) != "" {
		edge = look.palette.FailedText
	}
	giodraw.StrokeRRect(gtx, edge, bounds, radius, dp(gtx, theme.BorderWidth))
	call.Add(gtx.Ops)
	if gtx.Focused(d.editor.Tag()) {
		focusRing(gtx, look.palette.Text, dims.Size, radius)
	}
	return dims
}

func (d *Dock) editorView(gtx layout.Context, env frame, look dockLook) layout.Dimensions {
	gtx.Constraints.Min.X = gtx.Constraints.Max.X
	gtx.Constraints.Min.Y = 0
	gtx.Constraints.Max.Y = min(gtx.Constraints.Max.Y, dp(gtx, theme.FieldHeight)*fieldGrowth)
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions {
		return d.editor.Layout(gtx, platform.EditorStyle{
			Shaper:    env.Shaper,
			Font:      giofont.Font{Typeface: platform.ProseFace, Weight: platform.FontWeight(theme.WeightText)},
			Size:      unit.Sp(theme.InputTextSize),
			Text:      giodraw.Material(gtx, look.palette.Text),
			Selection: giodraw.Material(gtx, look.palette.Line),
		})
	})
	defer clip.Rect{Max: dims.Size}.Push(gtx.Ops).Pop()
	semantic.LabelOp(d.label()).Add(gtx.Ops)
	semantic.DescriptionOp(d.help(gtx, env)).Add(gtx.Ops)
	event.Op(gtx.Ops, &d.attach.pasteTag)
	call.Add(gtx.Ops)
	d.recordEditor(gtx, dims.Size, env)
	return dims
}

func (d *Dock) recordEditor(gtx layout.Context, size image.Point, env frame) {
	n := platform.SemanticNode{ID: "message", Role: platform.RoleTextField, Name: d.label(), Description: d.help(gtx, env), Placeholder: "Write a message", Action: "edit"}
	access.Add(gtx, image.Rectangle{Max: size}, n, access.Actions{Editor: d.editor, Rejected: func(err error) { d.notice.fail(rejectionText(err)) }})
}

func (d *Dock) actions(gtx layout.Context, env frame, look dockLook) layout.Dimensions {
	var items []layout.Widget
	add := func(c control) {
		items = append(items, func(gtx layout.Context) layout.Dimensions { return c.layout(gtx, env.Shaper, look.palette, look.point) })
	}
	if d.host.PickImages != nil {
		add(control{click: &d.ui.attach, label: "Attach image", variant: variantQuiet, reason: d.attachBlock(env)})
	}
	if imagesRefused(env, d.draft.chips) {
		add(control{click: &d.ui.correct, label: "Change model", describe: string(errImagesRefused), variant: variantSecondary})
	}
	if running(env) {
		add(control{click: &d.ui.queue, label: "Queue follow-up", variant: variantQuiet, reason: sendBlock(env)})
		add(control{click: &d.ui.interrupt, label: "Interrupt", variant: variantDestructive})
	}
	return flow(gtx, theme.Space3, items...)
}

func (d *Dock) attachBlock(env frame) string {
	if env.Thread == (state.ThreadKey{}) {
		return ""
	}
	if accepts, known := env.view.AcceptsImages(env.Thread); known && !accepts {
		return string(errImagesRefused)
	}
	return ""
}
