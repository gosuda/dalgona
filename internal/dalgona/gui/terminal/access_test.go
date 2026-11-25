//go:build gui

package terminal

import (
	"image"
	"strings"
	"testing"

	"gioui.org/layout"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestAccessibleTerminalSeparatesScreenAndScrollback(t *testing.T) {
	r := replicaOf(t, 20, 5, "earlier\r\na\r\nb\r\nc\r\nd\r\ncurrent")
	ax := accesstest.New()
	size := image.Pt(200, 80)
	gtx := ax.Begin(layout.Context{Constraints: layout.Exact(size), Ops: new(op.Ops)})
	r.ReadableOutput().Register(gtx, OutputRegion{ID: "one", Size: size, Reveal: func(int64) {}})
	ax.End(t, gtx)
	history := ax.Find(t, "Retained scrollback", platform.RoleTerminal)
	screen := ax.Find(t, "Current screen", platform.RoleTerminal)
	if history.Parent == screen.Parent || history.Value != "earlier" || strings.Contains(screen.Value, "earlier") || !strings.Contains(screen.Value, "current") {
		t.Fatalf("wrong terminal regions: history %q screen %q", history.Value, screen.Value)
	}
}
