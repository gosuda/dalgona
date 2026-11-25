//go:build gui

package shell

import (
	"errors"
	"testing"

	"gioui.org/io/key"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestAccessibleWindowCommandRunsOnceInNextFrame(t *testing.T) {
	h := newHarness(t)
	h.ax = accesstest.New()
	v := state.Snapshot{}.View()
	h.settle(v)
	menu := h.ax.Find(t, "Menu", platform.RoleButton)
	h.ax.Request(t, menu, platform.ActionActivate)
	h.frame(v)
	if now := h.ax.Find(t, "Menu", platform.RoleButton); !now.Expandable || !now.Expanded {
		t.Fatalf("open menu state: %+v", now)
	}
	command := h.ax.Find(t, "New window", platform.RoleMenuItem)
	s := h.ax.Tree.Snapshot()
	h.ax.Request(t, command, platform.ActionActivate)
	if h.windows.opened != 0 || h.ax.Wakeups != 2 {
		t.Fatalf("before frame: %d windows opened, %d wakes", h.windows.opened, h.ax.Wakeups)
	}
	h.frame(v)
	if h.windows.opened != 1 {
		t.Fatalf("next frame opened %d windows, want one", h.windows.opened)
	}
	h.frame(v)
	if h.windows.opened != 1 {
		t.Fatalf("accessibility command ran again, opening %d windows", h.windows.opened)
	}
	stale := platform.AccessibilityIntent{Node: command.ID, Revision: s.Revision(), Action: platform.ActionActivate}
	if err := h.ax.Tree.Request(stale); !errors.Is(err, platform.ErrElementUnavailable) {
		t.Fatalf("stale command: %v", err)
	}
}

func TestAccessibilityFocusMovesGioFocusAndFollowsIt(t *testing.T) {
	h := newHarness(t)
	h.ax = accesstest.New()
	v := state.Snapshot{}.View()
	h.settle(v)
	menu := h.ax.Find(t, "Menu", platform.RoleButton)
	h.ax.Request(t, menu, platform.ActionFocus)
	h.frame(v)
	if got := h.ax.Tree.Snapshot().Focus(); got != menu.ID || !h.router.Source().Focused(&h.shell.apps.toggle) {
		t.Fatalf("focus intent focused %q, want menu %q in the snapshot and Gio", got, menu.ID)
	}
	h.router.Source().Execute(key.FocusCmd{Tag: &h.shell.nav.start})
	h.frame(v)
	got, ok := h.ax.Tree.Snapshot().Node(h.ax.Tree.Snapshot().Focus())
	if !ok || got.ID == menu.ID || got.Name != "Start thread" {
		t.Fatalf("new Gio focus was not shown in the snapshot: %+v", got)
	}
}

func TestAccessibleThreadIDSurvivesInsertedRows(t *testing.T) {
	h := newHarness(t)
	h.ax = accesstest.New()
	row := summary("stable", "Existing thread", "")
	s := state.Snapshot{
		Endpoints: []state.EndpointView{endpoint(local, "Local", client.PhaseConnected)},
		Lanes:     map[client.EndpointID]state.LaneView{local: loaded(row)},
	}
	h.settle(s.View())
	before := h.ax.Find(t, "Existing thread", platform.RoleButton)
	s.Lanes[local] = loaded(summary("inserted", "Inserted first", ""), row)
	h.frame(s.View())
	after := h.ax.Find(t, "Existing thread", platform.RoleButton)
	if before.ID != after.ID || before.Bounds.Empty() || after.Bounds.Empty() || before.Bounds.Min.Y == after.Bounds.Min.Y {
		t.Fatalf("thread identity or bounds did not follow insertion: before %+v, after %+v", before, after)
	}
	revision := h.ax.Tree.Snapshot().Revision()
	h.frame(s.View())
	if got := h.ax.Tree.Snapshot().Revision(); got != revision {
		t.Fatalf("unchanged frame changed revision from %d to %d", revision, got)
	}
}
