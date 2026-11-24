//go:build gui

package code

import (
	"strconv"
	"strings"

	"gioui.org/font"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

type View struct {
	Shaper *text.Shaper
	Face   font.Font
	Size   unit.Sp
	Colors theme.Palette
}

func (v View) materials(gtx layout.Context) map[Kind]op.CallOp {
	out := make(map[Kind]op.CallOp, Error+1)
	for kind := PlainText; kind <= Error; kind++ {
		macro := op.Record(gtx.Ops)
		paint.ColorOp{Color: kind.Color(v.Colors)}.Add(gtx.Ops)
		out[kind] = macro.Stop()
	}
	return out
}

func (v View) label(gtx layout.Context, mats map[Kind]op.CallOp, kind Kind, content string) layout.Dimensions {
	var line widget.Label
	line.MaxLines = 1
	line.LineHeight = unit.Sp(float32(v.Size) * theme.LeadingCode)
	return line.Layout(gtx, v.Shaper, v.Face, v.Size, content, mats[kind])
}

func LayoutCode(gtx layout.Context, v View, source string, block Block) layout.Dimensions {
	mats := v.materials(gtx)
	lines := strings.Split(source, "\n")
	children := make([]layout.FlexChild, 0, len(lines))
	base := 0
	span := 0
	for _, line := range lines {
		top := base
		bottom := base + len(line)
		var row []layout.FlexChild
		for span < len(block.Spans) && block.Spans[span].Start < bottom {
			s := block.Spans[span]
			from := max(s.Start, top)
			to := min(s.End, bottom)
			if from < to {
				text := source[from:to]
				kind := s.Kind
				row = append(row, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
					return v.label(gtx, mats, kind, text)
				}))
			}
			if s.End <= bottom {
				span++
			} else {
				break
			}
		}
		if len(row) == 0 {
			row = append(row, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return v.label(gtx, mats, PlainText, "")
			}))
		}
		rowCopy := row
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return layout.Flex{Axis: layout.Horizontal}.Layout(gtx, rowCopy...)
		}))
		base = bottom + 1
	}
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func LayoutDiff(gtx layout.Context, v View, file File, keep int) layout.Dimensions {
	status, ok := fileStatus(file)
	if ok {
		mats := v.materials(gtx)
		return v.label(gtx, mats, PlainText, status)
	}
	mats := v.materials(gtx)
	width := gutterWidth(file)
	var children []layout.FlexChild
	for _, h := range file.Hunks {
		header := "@@ -" + hunkRange(h.OldStart, h.OldCount) + " +" + hunkRange(h.NewStart, h.NewCount) + " @@"
		children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return v.label(gtx, mats, Comment, header)
		}))
		for _, row := range hunkRows(h, keep) {
			row := row
			children = append(children, layout.Rigid(func(gtx layout.Context) layout.Dimensions {
				return layoutDiffRow(gtx, v, mats, width, row)
			}))
		}
	}
	return layout.Flex{Axis: layout.Vertical}.Layout(gtx, children...)
}

func layoutDiffRow(gtx layout.Context, v View, mats map[Kind]op.CallOp, width int, row Row) layout.Dimensions {
	if row.IsGap {
		return v.label(gtx, mats, Comment, gapText(row))
	}
	mark, kind := diffMark(row.Line)
	gutter := gutterText(row.Line, width)
	return layout.Flex{Axis: layout.Horizontal}.Layout(gtx,
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return v.label(gtx, mats, Comment, gutter)
		}),
		layout.Rigid(func(gtx layout.Context) layout.Dimensions {
			return v.label(gtx, mats, kind, mark+row.Line.Text)
		}),
	)
}

func diffMark(line Line) (string, Kind) {
	switch line.Op {
	case AddOp:
		return "+", Added
	case DeleteOp:
		return "-", Removed
	case ContextOp:
		return " ", PlainText
	}
	return " ", PlainText
}

func gutterText(line Line, width int) string {
	return padNumber(line.Old, width) + " " + padNumber(line.New, width) + " "
}

func gutterWidth(file File) int {
	top := 0
	for _, h := range file.Hunks {
		if end := h.OldStart + h.OldCount; end > top {
			top = end
		}
		if end := h.NewStart + h.NewCount; end > top {
			top = end
		}
	}
	return len(strconv.Itoa(top + 1))
}

func padNumber(n, width int) string {
	if n == 0 {
		return strings.Repeat(" ", width)
	}
	s := strconv.Itoa(n)
	if len(s) < width {
		return strings.Repeat(" ", width-len(s)) + s
	}
	return s
}

func gapText(row Row) string {
	return "… " + strconv.Itoa(row.GapOldCount) + " unchanged lines …"
}

func fileStatus(file File) (string, bool) {
	switch file.Kind {
	case Omitted:
		return "Diff omitted: " + file.Omitted, true
	case Binary:
		return "Binary files " + file.OldName + " and " + file.NewName + " differ", true
	case Rename:
		return "Renamed from " + file.OldName + " to " + file.NewName, true
	case Missing:
		return "Content unavailable for " + file.NewName, true
	case Modify, Add, Delete:
		return "", false
	default:
		return "", false
	}
}
