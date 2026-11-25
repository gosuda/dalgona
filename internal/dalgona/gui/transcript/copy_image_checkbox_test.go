//go:build gui

package transcript

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/document"
	"github.com/gosuda/dalgona/internal/dalgona/gui/figure"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestCopySelectAllLoadsCompleteSource(t *testing.T) {
	withTranscript(t, spillTurn, func(t *testing.T, h *harness) {
		h.tap(h.textPoint("row 30000"), 0)
		h.keyPress("A", key.ModShortcut)
		h.keyPress("C", key.ModShortcut)
		pane := h.tr.panes[h.key]
		t.Logf("selected=%v notice=%q pending=%v", pane.sel.Selected, h.tr.notice, h.tr.pendingCopy != nil)
		if h.tr.pendingCopy == nil {
			t.Fatalf("copying a tailed selection did not park a full-source load (selected=%v notice=%q)", pane.sel.Selected, h.tr.notice)
		}
		h.settleUntil("Copied the selection.")
		_, content, ok := h.router.WriteClipboard()
		if !ok {
			t.Fatalf("copy wrote nothing; notice %q", h.tr.notice)
		}
		var full strings.Builder
		for i := 1; i <= 30000; i++ {
			fmt.Fprintf(&full, "row %05d\n", i)
		}
		if string(content) != full.String() {
			head := string(content)
			if len(head) > 40 {
				head = head[:40]
			}
			t.Fatalf("copy wrote %d bytes starting %q, want the complete %d-byte spill", len(content), head, full.Len())
		}
	})
}

func TestRecordedImageStaysChargedSavesAndShowsFullSize(t *testing.T) {
	sup, err := figure.NewSupervisor(figure.SupervisorConfig{Fonts: figureFonts()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(sup.Close)
	woke := make(chan struct{}, 64)
	dir := t.TempDir()
	deps := Deps{Figures: figure.NewHub(sup, func() {
		select {
		case woke <- struct{}{}:
		default:
		}
	}), Save: func(context.Context, platform.DialogRequest) (platform.DialogResult, error) {
		return platform.DialogResult{Files: []string{filepath.Join(dir, "image-1.png")}}, nil
	}}
	data := pngOf(t)
	turns := func(d *daemon) []rpc.HistoryTurn {
		d.images["i1#0"] = data
		return commandTurn(d)[:1]
	}
	withTranscriptLive(t, deps, turns, func(t *testing.T, h *harness) {
		waitForImageRenderer(t, woke)
		h.settle()
		if got := sup.Pixels().Used(); got == 0 {
			t.Fatal("the thumbnail paint charges nothing against the shared pixel budget")
		}
		img := h.tr.panes[h.key].rows[identity(h.key, "u1", "i1")].images[0]
		if img.picture == nil || !img.picture.Live() {
			t.Fatal("the thumbnail has no live cache-owned paint")
		}
		h.click("View full size for Image 1")
		h.settle()
		if img.fullPicture == nil {
			t.Fatal("the full-size viewer kept no paint")
		}
		checkSavedImage(t, h, filepath.Join(dir, "image-1.png"), data)
	})
}

func TestTaskCheckboxNodeAnnouncesState(t *testing.T) {
	for _, tc := range []struct {
		task    document.Task
		checked bool
		state   string
	}{
		{document.TaskOpen, false, "unchecked"},
		{document.TaskDone, true, "checked"},
	} {
		var out accessibleRow
		out.checkbox(document.Block{Task: tc.task}, "block", "")
		if len(out.parts) != 1 {
			t.Fatalf("task %v produced %d nodes, want one checkbox", tc.task, len(out.parts))
		}
		node := out.parts[0].node
		if node.Role != platform.RoleCheckBox || node.Checked != tc.checked || !node.Disabled {
			t.Fatalf("checkbox node %+v, want checked=%v disabled", node, tc.checked)
		}
		if node.Description != "Task "+tc.state {
			t.Fatalf("checkbox description %q, want the %s state", node.Description, tc.state)
		}
	}
}

func TestCopyImageWritesStoredBytesToClipboard(t *testing.T) {
	var copied []byte
	deps, woke := imageClipboardDeps(t, func(data []byte) error {
		copied = append([]byte(nil), data...)
		return nil
	})
	data := pngOf(t)
	turns := func(d *daemon) []rpc.HistoryTurn {
		d.images["i1#0"] = data
		return commandTurn(d)[:1]
	}
	withTranscriptLive(t, deps, turns, func(t *testing.T, h *harness) {
		waitForImageRenderer(t, woke)
		h.frame()
		h.click("Copy image 1")
		waitFor(t, "the original clipboard PNG", func() bool {
			h.frame()
			return copied != nil
		})
		if string(copied) != string(data) {
			t.Fatalf("Copy image wrote %d bytes, want the %d stored bytes", len(copied), len(data))
		}
		if h.tr.notice != "Copied image 1." {
			t.Fatalf("notice %q, want the copied confirmation", h.tr.notice)
		}
	})
}
