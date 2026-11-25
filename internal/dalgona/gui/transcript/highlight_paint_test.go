//go:build gui

package transcript

import (
	"context"
	"image/color"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/unit"

	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document/code"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
)

func TestTranscriptPaintsRendererHighlightAndRetainsPlainCodeOnFailure(t *testing.T) {
	t.Run("renderer-child", func(t *testing.T) { runHighlightPaint(t, "") })
	t.Run("renderer-refused", func(t *testing.T) { runHighlightPaint(t, filepath.Join(t.TempDir(), "missing-renderer")) })
}

func runHighlightPaint(t *testing.T, executable string) {
	t.Helper()
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Executable: executable, Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	woke := make(chan struct{}, 1)
	hub := figure.NewHub(sup, func() {
		select {
		case woke <- struct{}{}:
		default:
		}
	})
	const line = `var greeting = "hello"`
	withTranscriptLive(t, Deps{Figures: hub}, assistantTurn("```go\n"+line+"\n```\n"), func(t *testing.T, h *harness) {
		ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
		defer cancel()
		select {
		case <-woke:
		case <-ctx.Done():
			t.Fatal("the renderer outcome did not reach the transcript")
		}
		h.frame()
		assertHighlightPaint(t, h, line, executable)
	})
}

func assertHighlightPaint(t *testing.T, h *harness, line, executable string) {
	t.Helper()
	pl := h.placedText(line)
	var painted []code.Kind
	palette := document.Palette{Token: func(kind code.Kind) color.NRGBA {
		painted = append(painted, kind)
		return color.NRGBA{R: 255, A: 255}
	}}
	gtx := layout.Context{Ops: new(op.Ops), Metric: unit.Metric{PxPerDp: 1, PxPerSp: 1}}
	pl.layout.Paint(gtx, h.shaper, palette, document.Span{})
	if executable != "" {
		if len(painted) != 0 || !strings.Contains(h.tr.notice, "Code highlighting failed") || !h.has(line) {
			t.Fatalf("failed highlight did not leave complete plain code and a notice: colors %v, notice %q", painted, h.tr.notice)
		}
		return
	}
	keyword := slices.Index(painted, code.Keyword)
	literal := slices.Index(painted, code.String)
	if keyword < 0 || literal <= keyword {
		t.Fatalf("paint selected colors %v; want keyword color before the string literal color", painted)
	}
}
