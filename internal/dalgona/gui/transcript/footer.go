//go:build gui

package transcript

import (
	"context"
	json "encoding/json/v2"
	"image/color"
	"io"
	"slices"
	"strings"

	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	panelRevert = "revert"
	panelNew    = "new"
)

type sourceActions struct {
	menu, copyMarkdown, saveMarkdown, rawCode widget.Clickable
	show                                      bool
	code                                      map[int]*widget.Clickable
}

type diffActions struct {
	split, full bool
	horizontal  [2]float32
	maxWidth    [2]float32
	nodes       map[diffPoint]*diffControls
}

type diffPoint struct {
	file, hunk int
}

type diffControls struct {
	previousFile, nextFile, previousHunk, nextHunk widget.Clickable
	copyPatch, openFile, full, mode                widget.Clickable
	scroll                                         [4]widget.Clickable
}

type requestTarget struct {
	ThreadID string `json:"threadId"`
	TurnID   string `json:"turnId"`
	ItemID   string `json:"itemId"`
}

func (t *Transcript) trackRequests(tv *state.ThreadView) {
	clear(t.waiting)
	for _, rq := range tv.Requests {
		if rq.Answered || len(rq.Params) == 0 {
			continue
		}
		target, ok := t.targets[rq.Key]
		if !ok {
			if json.Unmarshal(rq.Params, &target) != nil {
				target = requestTarget{}
			}
			t.targets[rq.Key] = target
		}
		if target.ItemID != "" {
			t.waiting[state.ItemKey{Turn: target.TurnID, Item: target.ItemID}] = true
		}
	}
}

func (t *Transcript) trackResolved(tv *state.ThreadView) {
	clear(t.timedOut)
	for _, rs := range tv.Resolved {
		if rs.Resolution != statusTimedOut || rs.Item == "" {
			continue
		}
		key := state.ItemKey{Item: rs.Item}
		if target, ok := t.targets[rs.Key]; ok && target.ItemID == rs.Item {
			key.Turn = target.TurnID
		}
		t.timedOut[key] = true
	}
}

func (t *Transcript) awaiting(e Entry) bool {
	return t.waiting[e.Item.Key]
}

func (t *Transcript) expired(e Entry) bool {
	return t.timedOut[e.Item.Key] || t.timedOut[state.ItemKey{Item: e.Item.Key.Item}]
}

func (t *Transcript) footer(gtx layout.Context, f *frame) layout.Dimensions {
	env, r := f.env, f.row
	var lines []layout.FlexChild
	add := func(w layout.Widget) { lines = append(lines, layout.Rigid(w)) }
	lines = t.contentStatus(env, r.sheet, lines)
	lines = append(lines, t.userImages(gtx, env, r)...)
	if words := keywordLine(r.sheet.keywords); words != "" {
		add(t.caption(env, words, t.palette.Dim))
	}
	if note := t.fieldStatus(env, r.field); note != nil {
		add(note)
	}
	if review := r.entry.Item.Review; r.synth == synthNone && review != nil {
		add(t.reviewLine(env, review))
	}
	actions := t.actions(gtx, f)
	if len(actions) > 0 {
		add(func(gtx layout.Context) layout.Dimensions {
			return layout.Inset{Top: unit.Dp(theme.Space1)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
				return layout.Flex{Spacing: layout.SpaceEnd}.Layout(gtx, actions...)
			})
		})
	}
	if r.doc.Format() == document.FormatMarkdown && r.sourceActions.show {
		add(func(gtx layout.Context) layout.Dimensions { return t.markdownActions(gtx, f) })
	}
	if len(lines) == 0 {
		return layout.Dimensions{}
	}
	return layout.Inset{Top: unit.Dp(theme.Space1)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{Axis: layout.Vertical}.Layout(gtx, lines...)
	})
}

func (t *Transcript) contentStatus(env shell.Env, s sheet, lines []layout.FlexChild) []layout.FlexChild {
	if s.wait {
		lines = append(lines, layout.Rigid(t.caption(env, "Loading content", t.palette.Dim)))
	}
	if s.failed != nil {
		lines = append(lines, layout.Rigid(t.caption(env, "Content could not load: "+s.failed.err.Error(), t.palette.FailedText)))
	}
	if s.projectionErr != nil {
		lines = append(lines, layout.Rigid(t.caption(env, "Content could not be displayed: "+s.projectionErr.Error(), t.palette.FailedText)))
	}
	if s.note != "" {
		lines = append(lines, layout.Rigid(t.caption(env, s.note, t.palette.Dim)))
	}
	return lines
}

func (t *Transcript) fieldStatus(env shell.Env, f *viewField) layout.Widget {
	if f == nil {
		return nil
	}
	text := f.state()
	if text == "" {
		return nil
	}
	c := t.palette.Dim
	if f.err != nil {
		c = t.palette.FailedText
	}
	return t.caption(env, text, c)
}

func (t *Transcript) caption(env shell.Env, text string, c color.NRGBA) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return label(gtx, env.Shaper, style{face: caption(c).face, weight: theme.WeightText, size: theme.SizeSmall, leading: theme.LeadingDescription, color: c}, text)
	}
}

func (t *Transcript) reviewLine(env shell.Env, review *state.Review) layout.Widget {
	st := prose(t.palette.Dim)
	st.size = theme.SizeSmall
	switch review.Verdict {
	case "flagged":
		st.color = t.palette.WarningText
	case "error":
		st.color, st.weight = t.palette.FailedText, theme.WeightStrong
	}
	return func(gtx layout.Context) layout.Dimensions {
		return label(gtx, env.Shaper, st, review.Summary)
	}
}

func keywordLine(keywords []keyword) string {
	if len(keywords) == 0 {
		return ""
	}
	words := make([]string, 0, len(keywords))
	for _, k := range keywords {
		words = append(words, k.Word)
	}
	return "Keywords: " + strings.Join(words, ", ")
}

func (t *Transcript) actions(gtx layout.Context, frame *frame) []layout.FlexChild {
	env, r := frame.env, frame.row
	var out []layout.FlexChild
	add := func(click *widget.Clickable, name string, busy bool, act func()) {
		if click.Clicked(gtx) && !busy {
			act()
		}
		if !busy {
			frame.addFocus(click)
		}
		out = append(out, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return layout.Inset{Right: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
				return t.ctrl.button(gtx, click, name, busy)
			})
		}))
	}
	if s := r.sheet; s.failed != nil {
		add(&r.retry, "Retry", false, func() { t.loader.retry(s.failed, env.Submit) })
	}
	t.rowActions(gtx, env, r, add)
	if f := r.field; f != nil {
		fieldActions(add, f)
		if r.sheet.projectionErr != nil && f.format == document.FormatCode {
			add(&r.sourceActions.rawCode, "Copy code", false, func() { t.copyReviewCode(gtx, f) })
		}
	}
	if r.doc.Format() == document.FormatMarkdown {
		add(&r.sourceActions.menu, "Document actions", false, func() {
			r.sourceActions.show = !r.sourceActions.show
			gtx.Execute(op.InvalidateCmd{})
		})
	}
	return out
}

func (t *Transcript) rowActions(gtx layout.Context, env shell.Env, r *row, add func(*widget.Clickable, string, bool, func())) {
	switch r.synth {
	case synthNone:
		t.entryActions(gtx, env, r, add)
	case synthOutcome:
		if r.latest && r.turn.Status == "failed" {
			add(&r.fresh, "New thread", false, func() { env.Open(shell.Route{Panel: panelNew, Thread: env.Thread}) })
		}
	case synthHistory:
		historyActions(env, r, add)
	case synthReview, synthPlan, synthChildren, synthField:
	}
}

func (t *Transcript) entryActions(gtx layout.Context, env shell.Env, r *row, add func(*widget.Clickable, string, bool, func())) {
	if r.sheet.full || r.entry.Kind == KindOther {
		add(&r.open, "Open full text", false, func() { t.openReview(gtx, r, &r.open) })
	}
	if r.entry.Kind == KindUser {
		add(&r.rewind, "Rewind to here", false, func() { env.Open(shell.Route{Panel: panelRevert, Thread: env.Thread, Turn: r.entry.Turn}) })
	}
}

func historyActions(env shell.Env, r *row, add func(*widget.Clickable, string, bool, func())) {
	tv, _ := env.View.Thread(env.Thread)
	busy := tv != nil && tv.LoadingOlder
	name := "Load earlier turns"
	if busy {
		name = historyLoading
	}
	add(&r.older, name, busy, func() { env.Submit(state.LoadOlder{Thread: env.Thread}) })
}

func fieldActions(add func(*widget.Clickable, string, bool, func()), f *viewField) {
	switch {
	case f.err != nil:
		add(&f.retry, "Retry", false, func() { f.err, f.asked, f.pull = nil, 0, true })
	case f.hasRef && !f.complete() && f.next > f.base:
		add(&f.more, "Load more", f.asked != 0, func() { f.pull = true })
	}
	if f.base > 0 {
		add(&f.restart, "Reload from start", false, f.reset)
	}
}

func (t *Transcript) markdownActions(gtx layout.Context, f *frame) layout.Dimensions {
	r := f.row
	f.addFocus(&r.sourceActions.copyMarkdown)
	f.addFocus(&r.sourceActions.saveMarkdown)
	return layout.Flex{}.Layout(gtx,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			if r.sourceActions.copyMarkdown.Clicked(gtx) {
				t.exportMarkdown(gtx, r, nil)
			}
			return t.ctrl.button(gtx, &r.sourceActions.copyMarkdown, "Copy Markdown", false)
		}),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			if r.sourceActions.saveMarkdown.Clicked(gtx) {
				t.exportMarkdown(gtx, r, func(write func(io.Writer) error) {
					release, ok := t.leaseExportSource(r.doc)
					if !ok {
						t.notice = "Export failed: the complete Markdown source could not be held."
						return
					}
					t.beginSave(saveRequest{title: "Markdown", filename: "document.md", write: write, release: release})
				})
			}
			return t.ctrl.button(gtx, &r.sourceActions.saveMarkdown, "Save Markdown", false)
		}),
	)
}

func (t *Transcript) exportMarkdown(gtx layout.Context, r *row, save func(func(io.Writer) error)) {
	if !sourceReady(r) || r.doc.Open() {
		t.notice = "Markdown is incomplete; wait for the document to finish loading."
		return
	}
	scope := document.NewScope(r.id.Endpoint, r.id.Thread)
	if _, err := scope.Put(r.doc); err != nil {
		t.notice = "Markdown export failed: " + err.Error()
		return
	}
	sel, err := scope.All()
	if err != nil {
		t.notice = "Markdown export failed: " + err.Error()
		return
	}
	if save != nil {
		save(func(w io.Writer) error {
			return scope.WriteMarkdown(context.Background(), sel, residentPager{}, w)
		})
		return
	}
	if err := scope.CopyMarkdown(context.Background(), gtx.Source, sel, residentPager{}); err != nil {
		t.notice = "Copy failed: " + err.Error()
		return
	}
	t.notice = "Copied the Markdown source."
}

func sourceReady(r *row) bool {
	if r.sheet.incomplete() || r.doc.Source() != r.sheet.source {
		return false
	}
	if r.field != nil {
		return r.field.complete() && r.field.base == 0
	}
	for _, b := range r.sheet.sources {
		if !b.complete() || b.start != 0 {
			return false
		}
	}
	return true
}

func (t *Transcript) codeAction(f *frame, b document.Block) {
	r := f.row
	button := r.sourceActions.code[f.ref.Block]
	if button == nil {
		if r.sourceActions.code == nil {
			r.sourceActions.code = make(map[int]*widget.Clickable)
		}
		button = new(widget.Clickable)
		r.sourceActions.code[f.ref.Block] = button
	}
	f.addFocus(button)
	f.stack(func(gtx layout.Context) layout.Dimensions {
		if button.Clicked(gtx) {
			t.copyCode(gtx, r, f.ref.Block, b)
		}
		return t.ctrl.button(gtx, button, "Copy code", false)
	})
}

func (t *Transcript) copyCode(gtx layout.Context, r *row, block int, b document.Block) {
	switch {
	case r.doc.Format() == document.FormatCode && r.synth == synthNone:
		t.copyOutput(gtx, r)
	case r.field != nil && r.doc.Format() == document.FormatCode:
		t.copyReviewCode(gtx, r.field)
	case !sourceReady(r) || b.Key.Provisional:
		t.notice = "Copy code needs the complete source; wait for it to finish loading."
	case b.Body.Len() > document.MaxCopyBytes:
		t.notice = "Copy failed: the code is too large for the clipboard."
	default:
		t.copySourceCode(gtx, r.doc, block)
	}
}

func (t *Transcript) copyReviewCode(gtx layout.Context, f *viewField) {
	if !f.complete() || f.base != 0 || uint64(len(f.data)) != f.total {
		t.notice = "Copy code needs the complete source; wait for it to finish loading."
		return
	}
	if len(f.suffix) > document.MaxCopyBytes-len(f.data) {
		t.notice = "Copy failed: the code is too large for the clipboard."
		return
	}
	t.copyExact(gtx, string(f.data)+f.suffix, "code")
}

func (t *Transcript) copySourceCode(gtx layout.Context, doc *document.Document, block int) {
	if err := doc.CopySource(gtx.Source, block); err != nil {
		t.notice = "Copy failed: " + err.Error()
		return
	}
	t.notice = "Copied the code."
}

func (t *Transcript) copyOutput(gtx layout.Context, r *row) {
	read := reader{loader: t.loader, thread: r.entry.Thread, item: r.entry.Item}
	ft := read.output("aggregatedOutput")
	if r.entry.Kind == KindTool {
		ft = toolOutput(sheetInput{entry: r.entry, read: read})
	}
	if completeField(ft) && !r.entry.Live() {
		t.copyExact(gtx, ft.text, "code")
		return
	}
	t.notice = "Copy code needs the complete source; open Full text for this output."
}

func (t *Transcript) copyExact(gtx layout.Context, source, kind string) {
	if err := platform.WriteText(gtx.Source, source); err != nil {
		t.notice = "Copy failed: " + err.Error()
		return
	}
	t.notice = "Copied the " + kind + "."
}

type diffJump struct {
	row   *row
	point diffPoint
}

func (t *Transcript) diffToolbar(f *frame, line int) {
	r := f.row
	d := r.sheet.diffLines[line]
	point := diffPoint{file: d.file, hunk: d.hunk}
	ui := &r.diffActions
	if ui.nodes == nil {
		ui.nodes = make(map[diffPoint]*diffControls)
	}
	c := ui.nodes[point]
	if c == nil {
		c = new(diffControls)
		ui.nodes[point] = c
	}
	gtx := f.gtx
	jump := func(point diffPoint) {
		f.pane.diffJump = &diffJump{row: r, point: point}
		gtx.Execute(op.InvalidateCmd{})
	}
	if d.role == roleFile {
		t.diffFileActions(f, d, c, jump)
	}
	if len(r.sheet.diffHunks) > 0 {
		t.diffHunkActions(f, line, c, jump)
	}
}

func (t *Transcript) diffFileActions(f *frame, d diffLine, c *diffControls, jump func(diffPoint)) {
	r := f.row
	f.stack(func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{}.Layout(gtx,
			t.diffButton(f, &c.previousFile, "Previous file", d.file == 0, func() { jump(diffPoint{file: d.file - 1}) }),
			t.diffButton(f, &c.nextFile, "Next file", d.file+1 >= len(r.sheet.diffPaths), func() { jump(diffPoint{file: d.file + 1}) }),
			t.diffButton(f, &c.copyPatch, "Copy patch", false, func() { t.copyFilePatch(gtx, r, d.file) }),
			t.diffButton(f, &c.openFile, "Open file", r.sheet.diffPaths[d.file] == "", func() {
				thread := r.entry.Thread
				if r.field != nil {
					thread = r.field.thread
				}
				f.env.Open(shell.Route{Panel: "files", Thread: thread, Context: r.sheet.diffPaths[d.file]})
			}),
		)
	})
	mode := "Side-by-side"
	if r.diffActions.split {
		mode = "Unified"
	}
	f.stack(func(gtx layout.Context) layout.Dimensions {
		buttons := []layout.FlexChild{t.diffButton(f, &c.mode, mode, false, func() {
			r.diffActions.split = !r.diffActions.split
			r.diffActions.full = true
			if f.pane.sel.Selection.Focus.Document == r.id || f.pane.sel.Selection.Anchor.Document == r.id {
				f.pane.sel.Selected = false
			}
			jump(diffPoint{file: d.file})
		})}
		if r.sheet.diffLimited {
			buttons = append(buttons, t.diffButton(f, &c.full, "Show all hunks", false, func() {
				r.diffActions.full = true
				jump(diffPoint{file: d.file})
			}))
		}
		return layout.Flex{}.Layout(gtx, buttons...)
	})
	if r.diffActions.split {
		t.diffScrollButtons(f, c)
	}
}

func (t *Transcript) diffHunkActions(f *frame, line int, c *diffControls, jump func(diffPoint)) {
	r := f.row
	hunks := r.sheet.diffHunks
	at, isHunk := slices.BinarySearch(hunks, line)
	prev, next := at-1, at
	if isHunk {
		next++
	}
	goHunk := func(i int) {
		d := r.sheet.diffLines[hunks[i]]
		jump(diffPoint{file: d.file, hunk: d.hunk})
	}
	f.stack(func(gtx layout.Context) layout.Dimensions {
		return layout.Flex{}.Layout(gtx,
			t.diffButton(f, &c.previousHunk, "Previous hunk", prev < 0, func() { goHunk(prev) }),
			t.diffButton(f, &c.nextHunk, "Next hunk", next >= len(hunks), func() { goHunk(next) }),
		)
	})
}

func (t *Transcript) diffButton(f *frame, click *widget.Clickable, name string, disabled bool, act func()) layout.FlexChild {
	if !disabled {
		f.addFocus(click)
	}
	if click.Clicked(f.gtx) && !disabled {
		act()
	}
	return layout.Rigid(func(gtx layout.Context) layout.Dimensions {
		if disabled {
			gtx = gtx.Disabled()
		}
		return layout.Inset{Right: unit.Dp(theme.Space1)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return t.ctrl.button(gtx, click, name, disabled)
		})
	})
}

func (t *Transcript) copyFilePatch(gtx layout.Context, r *row, index int) {
	changes, ready, err := t.rawPatchChanges(r)
	if err != nil {
		t.notice = "Copy patch failed: " + err.Error()
		return
	}
	if !ready || index >= len(changes) || changes[index].Path != r.sheet.diffPaths[index] {
		t.notice = "Copy patch needs the complete source for this change."
		return
	}
	c := changes[index]
	if c.Omitted {
		t.notice = "Copy patch failed: " + errPatchOmitted.Error()
		return
	}
	t.copyExact(gtx, c.Diff, "patch")
}

func (t *Transcript) rawPatchChanges(r *row) ([]change, bool, error) {
	if f := r.field; f != nil {
		if f.base != 0 || !f.complete() {
			return nil, false, nil
		}
		changes, err := parseChanges(f.data)
		return changes, true, err
	}
	changes, ft, err := readChanges(reader{loader: t.loader, thread: r.entry.Thread, item: r.entry.Item})
	return changes, completeField(ft), err
}

func (t *Transcript) reviewDiffSheet(r *row, f *viewField) sheet {
	s := sheet{format: f.format}
	changes, err := parseChanges(f.data)
	if err != nil {
		s.source = "The file changes could not be read: " + err.Error()
		return s
	}
	return diffSheet(s, changes, sheetInput{diffSplit: r.diffActions.split, diffFull: true})
}

func (d *diffActions) scroll(side int, dx, visible float32) {
	d.horizontal[side] = min(max(d.horizontal[side]+dx, 0), max(d.maxWidth[side]-visible, 0))
}

func (t *Transcript) diffScrollButtons(f *frame, c *diffControls) {
	f.stack(func(gtx layout.Context) layout.Dimensions {
		room := max(float32(f.column/2-min(gutterCell(gtx), f.column/4)), 1)
		ui := &f.row.diffActions
		names := [...]string{"Old ←", "Old →", "New ←", "New →"}
		var buttons [4]layout.FlexChild
		for i, name := range names {
			side := i / 2
			step := room * 0.75
			disabled := ui.horizontal[side] >= max(ui.maxWidth[side]-room, 0)
			if i%2 == 0 {
				step = -step
				disabled = ui.horizontal[side] <= 0
			}
			buttons[i] = t.diffButton(f, &c.scroll[i], name, disabled, func() {
				ui.scroll(side, step, room)
				gtx.Execute(op.InvalidateCmd{})
			})
		}
		return layout.Flex{}.Layout(gtx, buttons[:]...)
	})
}
