//go:build gui && guiintegration

package main

import (
	"testing"

	"gioui.org/app"
	"gioui.org/io/system"

	"github.com/gosuda/dalgona/internal/dalgona/gui/manage"
)

func TestRealWindowClosesWithoutPendingDecisions(t *testing.T) {
	a := newApplication(t.Context(), t.TempDir(), nil)
	a.attachHub(manage.NewHub(t.Context(), manage.Deps{}))
	win, err := newWindow(a, "window-1")
	if err != nil {
		t.Fatal(err)
	}
	a.open[win.id] = win
	frames := 0
	for {
		e := win.w.Event()
		if _, ok := e.(app.FrameEvent); ok {
			frames++
			if frames == 1 {
				win.w.Perform(system.ActionClose)
			}
		}
		if done, err := win.handle(e); done {
			if err != nil {
				t.Fatalf("window failed: %v", err)
			}
			break
		}
	}
	if frames == 0 {
		t.Fatal("the window closed before its first frame")
	}
}
