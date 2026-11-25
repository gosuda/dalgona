//go:build gui

package decision

import (
	"fmt"
	"image"
	"strconv"
	"strings"

	"gioui.org/layout"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type fileDiff struct {
	change rpc.FileChange
	file   code.File
	err    error
	digits int
}

func (c *card) loadChanges(owner *state.ThreadView) {
	changes, ok := changesOf(owner, c.details.file)
	if !ok {
		c.loaded = false
		return
	}
	if c.loaded && len(changes) == len(c.changes) && (len(changes) == 0 || &changes[0] == &c.changes[0]) {
		return
	}
	c.changes, c.loaded, c.diffs = changes, true, c.diffs[:0]
	for _, change := range changes {
		file, err := code.ParseUnified(change.Diff)
		c.diffs = append(c.diffs, fileDiff{change: change, file: file, err: err, digits: lineDigits(file)})
	}
}

func changesOf(owner *state.ThreadView, params rpc.FileChangeRequestApprovalParams) ([]rpc.FileChange, bool) {
	if owner == nil {
		return nil, false
	}
	for _, turn := range owner.Turns {
		if turn.ID != params.TurnID {
			continue
		}
		for _, item := range turn.Items {
			if item.Key.Item == params.ItemID && item.Changes != nil {
				return item.Changes, true
			}
		}
	}
	return nil, false
}

func lineDigits(file code.File) int {
	top := 0
	for _, h := range file.Hunks {
		top = max(top, h.OldStart+h.OldCount, h.NewStart+h.NewCount)
	}
	return len(strconv.Itoa(top))
}

func (d *Dock) fileItems(ps paints, f frame) []layout.Widget {
	c := f.card
	items := compact(d.reason(ps, c.details.file.Reason), d.childItem(ps, f))
	if !c.loaded {
		return append(items, d.note(ps, "The proposed changes have not arrived in this window yet. Accept stays disabled until they do."))
	}
	for i := range c.diffs {
		items = append(items, d.diffItems(ps, &c.diffs[i])...)
	}
	return append(items, d.note(ps, fileGrantNote))
}

func (d *Dock) diffItems(ps paints, fd *fileDiff) []layout.Widget {
	items := []layout.Widget{d.changeMeta(ps, fd.change)}
	switch {
	case fd.change.Diff == "":
		return append(items, d.note(ps, "No diff was supplied for this file: it is over the diff limit or not text."))
	case fd.err != nil:
		n := platform.SemanticNode{ID: platform.NodeID(access.Path("diff", fd.change.Path)), Name: "Diff for " + fd.change.Path, Value: fd.change.Diff, Role: platform.RoleCode}
		return append(items, d.note(ps, fmt.Sprintf("This diff could not be parsed (%v), so it is shown exactly as sent.", fd.err)), codeText(ps, n, theme.SizeCaption))
	}
	for _, h := range fd.file.Hunks {
		items = append(items, d.hunkHeader(ps, h))
		for _, line := range h.Lines {
			items = append(items, d.diffLine(ps, line, fd.digits))
		}
	}
	return items
}

func (d *Dock) changeMeta(ps paints, change rpc.FileChange) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space3)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Horizontal, Alignment: layout.Middle, Spacing: layout.SpaceEnd}.Layout(gtx,
				layout.Rigid(func(gtx layout.Context) layout.Dimensions { return d.editTag(gtx, ps, change.Kind) }),
				layout.Rigid(layout.Spacer{Width: unit.Dp(theme.OptionGap)}.Layout),
				layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
					gtx.Constraints.Min.X = 0
					return drawText(gtx, ps.shaper, mono(ps.palette.Text, theme.SizeCaption), change.Path)
				}),
				layout.Rigid(layout.Spacer{Width: unit.Dp(theme.OptionGap)}.Layout),
				layout.Rigid(func(gtx layout.Context) layout.Dimensions { return d.counts(gtx, ps, change) }),
			)
		})
	}
}

func (d *Dock) editTag(gtx layout.Context, ps paints, kindName string) layout.Dimensions {
	padB, padI := dp(gtx, theme.Space1), dp(gtx, theme.OptionPaddingBlock)
	st := textStyle{face: platform.TitleFace, weight: theme.WeightTitle, size: theme.SizeCaption, leading: theme.LeadingTight, color: ps.palette.Text, maxLines: 1}
	call, dims := giodraw.Record(sized(gtx), func(gtx layout.Context) layout.Dimensions { return drawText(gtx, ps.shaper, st, kindName) })
	size := dims.Size.Add(image.Pt(2*padI, 2*padB))
	giodraw.FillRRect(gtx, ps.palette.Tone, image.Rectangle{Max: size}, dp(gtx, theme.RadiusEdit))
	giodraw.Place(gtx, image.Pt(padI, padB), call)
	return layout.Dimensions{Size: size}
}

func (d *Dock) counts(gtx layout.Context, ps paints, change rpc.FileChange) layout.Dimensions {
	added := mono(ps.palette.AddedText, theme.SizeSmall)
	added.weight, added.maxLines, added.wrap = theme.WeightStrong, 1, 0
	removed := mono(ps.palette.FailedText, theme.SizeSmall)
	removed.maxLines, removed.wrap = 1, 0
	return layout.Flex{Axis: layout.Horizontal}.Layout(gtx,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, added, "+"+strconv.Itoa(change.Additions))
		}),
		layout.Rigid(layout.Spacer{Width: unit.Dp(theme.Space2)}.Layout),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, removed, "−"+strconv.Itoa(change.Deletions))
		}),
	)
}

func (d *Dock) hunkHeader(ps paints, h code.Hunk) layout.Widget {
	header := fmt.Sprintf("@@ -%d,%d +%d,%d @@", h.OldStart, h.OldCount, h.NewStart, h.NewCount)
	return func(gtx layout.Context) layout.Dimensions {
		return layout.Inset{Top: unit.Dp(theme.Space2)}.Layout(gtx, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, mono(ps.palette.Dim, theme.SizeCaption), header)
		})
	}
}

func (d *Dock) diffLine(ps paints, line code.Line, digits int) layout.Widget {
	sign, ink, number := " ", ps.palette.Text, line.New
	switch line.Op {
	case code.AddOp:
		sign, ink = "+", ps.palette.AddedText
	case code.DeleteOp:
		sign, ink, number = "−", ps.palette.FailedText, line.Old
	case code.ContextOp:
	}
	numberText := strconv.Itoa(number)
	return func(gtx layout.Context) layout.Dimensions {
		pad, signWidth := dp(gtx, theme.DiffPadding), dp(gtx, theme.DiffSignWidth)
		numberWidth := max(dp(gtx, theme.DiffNumberWidth), d.advance(gtx, ps, digits))
		width := gtx.Constraints.Max.X
		codeGtx := sized(gtx)
		codeGtx.Constraints.Max.X = max(0, width-2*pad-numberWidth-signWidth)
		codeCall, codeDims := giodraw.Record(codeGtx, func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, mono(ink, theme.SizeCaption), line.Text)
		})
		height := max(dp(gtx, theme.DiffLineHeight), codeDims.Size.Y)
		numberCall, numberDims := giodraw.Record(sized(gtx), func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, mono(ps.palette.Dim, theme.SizeCaption), numberText)
		})
		signCall, signDims := giodraw.Record(sized(gtx), func(gtx layout.Context) layout.Dimensions {
			return drawText(gtx, ps.shaper, mono(ink, theme.SizeCaption), sign)
		})
		giodraw.Place(gtx, image.Pt(pad+numberWidth-numberDims.Size.X, (height-numberDims.Size.Y)/2), numberCall)
		giodraw.Place(gtx, image.Pt(pad+numberWidth+(signWidth-signDims.Size.X)/2, (height-signDims.Size.Y)/2), signCall)
		giodraw.Place(gtx, image.Pt(pad+numberWidth+signWidth, (height-codeDims.Size.Y)/2), codeCall)
		return layout.Dimensions{Size: image.Pt(width, height)}
	}
}

func (d *Dock) advance(gtx layout.Context, ps paints, digits int) int {
	_, dims := giodraw.Record(sized(gtx), func(gtx layout.Context) layout.Dimensions {
		return drawText(gtx, ps.shaper, mono(ps.palette.Dim, theme.SizeCaption), strings.Repeat("0", digits))
	})
	return dims.Size.X
}
