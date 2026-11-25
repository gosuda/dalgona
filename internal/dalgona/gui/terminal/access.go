//go:build gui

package terminal

import (
	"image"
	"strconv"

	"gioui.org/io/event"
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

type ReadableOutput struct {
	Screen     string
	Scrollback string
	First      int64
	ScreenTop  int64
}

func (r *Replica) ReadableOutput() ReadableOutput {
	first, end := r.Lines()
	top := r.ScreenTop()
	return ReadableOutput{Screen: r.readableRange(top, end), Scrollback: r.readableRange(first, top), First: first, ScreenTop: top}
}

func (r *Replica) readableRange(first, end int64) string {
	columns, _ := r.Size()
	return r.Text(Extent{Start: Point{Line: first}, End: Point{Line: end - 1, Column: columns - 1}})
}

type OutputRegion struct {
	ID     string
	Size   image.Point
	Tag    event.Tag
	Reveal func(int64)
}

func (o ReadableOutput) Register(gtx layout.Context, r OutputRegion) {
	n := platform.SemanticNode{ID: platform.NodeID(access.Path("terminal", r.ID)), Role: platform.RoleTerminal, Name: "Terminal " + r.ID}
	access.Group(gtx, n, access.Actions{Tag: r.Tag}, func(gtx layout.Context) layout.Dimensions {
		registerTerminalText(gtx, r, platform.SemanticNode{ID: "screen", Name: "Current screen"}, o.Screen, o.ScreenTop)
		registerTerminalText(gtx, r, platform.SemanticNode{ID: "scrollback", Name: "Retained scrollback"}, o.Scrollback, o.First)
		return layout.Dimensions{Size: r.Size}
	})
}

func registerTerminalText(gtx layout.Context, r OutputRegion, n platform.SemanticNode, value string, line int64) {
	n.Role = platform.RoleDocument
	a := access.Actions{Scroll: func() { r.Reveal(line) }}
	access.Group(gtx, n, a, func(gtx layout.Context) layout.Dimensions {
		for _, part := range platform.TextObjects(value) {
			text := platform.SemanticNode{ID: platform.NodeID(strconv.Itoa(part.Start)), Role: platform.RoleTerminal, Name: n.Name, Value: value[part.Start:part.End]}
			access.Offscreen(gtx, text, a)
		}
		return layout.Dimensions{}
	})
}
