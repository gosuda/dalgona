//go:build gui && guiintegration && (windows || darwin)

package desktop

import (
	"context"
	"sync"
	"testing"
	"time"

	"gioui.org/app"
	"gioui.org/io/system"
	"gioui.org/op"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

var ownedDialogRequests = map[string]platform.DialogRequest{
	"open files":    {Kind: platform.DialogOpenFiles, Title: "Attach files", Filters: []platform.FileFilter{{Name: "Images", Extensions: []string{"png", "jpeg"}}}},
	"choose folder": {Kind: platform.DialogChooseFolder, Title: "Choose workspace folder"},
	"save file":     {Kind: platform.DialogSaveFile, Title: "Export document", SuggestedName: "notes.md", Filters: []platform.FileFilter{{Name: "Markdown", Extensions: []string{"md"}}}},
}

type dialogWindow struct {
	window  *app.Window
	desktop *Window
	view    app.ViewEvent
	views   chan app.ViewEvent
	ended   chan struct{}
	close   func()
}

func openDialogWindow(t *testing.T) *dialogWindow {
	t.Helper()
	w := new(app.Window)
	h := &dialogWindow{window: w, desktop: New(w, platform.NewAccessibilityTree(4, w.Invalidate)), views: make(chan app.ViewEvent, 1), ended: make(chan struct{})}
	h.close = sync.OnceFunc(func() {
		w.Perform(system.ActionClose)
		select {
		case <-h.ended:
		case <-time.After(10 * time.Second):
			t.Error("the window did not close")
		}
	})
	go h.loop(t)
	t.Cleanup(h.close)
	select {
	case h.view = <-h.views:
	case <-time.After(10 * time.Second):
		t.Fatal("the window never reported a native view")
	}
	return h
}

func (h *dialogWindow) loop(t *testing.T) {
	defer close(h.ended)
	var ops op.Ops
	for {
		e := h.window.Event()
		if err := h.desktop.Handle(context.Background(), e); err != nil {
			t.Errorf("handle %T: %v", e, err)
		}
		switch e := e.(type) {
		case app.ViewEvent:
			h.offer(e)
		case app.FrameEvent:
			ops.Reset()
			e.Frame(&ops)
		case app.DestroyEvent:
			return
		}
	}
}

func (h *dialogWindow) offer(view app.ViewEvent) {
	if !view.Valid() {
		return
	}
	select {
	case h.views <- view:
	default:
	}
}

func TestOwnedDialogEndsWithItsWindow(t *testing.T) {
	for name, req := range ownedDialogRequests {
		t.Run(name, func(t *testing.T) {
			h := openDialogWindow(t)
			if err := h.desktop.ShowDialog(req); err != nil {
				t.Fatal(err)
			}
			select {
			case got := <-h.desktop.DialogOutcomes():
				t.Fatalf("the dialog ended before its window closed: %+v", got)
			case <-time.After(2 * time.Second):
			}
			h.close()
			awaitDialogEnd(t, h.desktop)
		})
	}
}

func awaitDialogEnd(t *testing.T, d *Window) {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for d.dialogOpen.Load() {
		select {
		case <-deadline:
			t.Fatal("the dialog kept running after its window closed")
		case <-time.After(10 * time.Millisecond):
		}
	}
	select {
	case got := <-d.DialogOutcomes():
		t.Fatalf("a dialog outcome reached a closed window: %+v", got)
	default:
	}
}
