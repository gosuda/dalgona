package platform

import (
	"errors"
	"slices"
	"sync/atomic"
	"testing"
)

func TestSnapshotKeepsReadingOrderAndOneRoot(t *testing.T) {
	snap, err := NewSemanticSnapshot(1, "send", validTree())
	if err != nil {
		t.Fatal(err)
	}
	if snap.Root() != "transcript" || snap.Focus() != "send" {
		t.Fatalf("root %q focus %q", snap.Root(), snap.Focus())
	}
	if got, want := snap.Children("transcript"), []NodeID{"heading", "composer", "token", "send"}; !slices.Equal(got, want) {
		t.Fatalf("children %v, want reading order %v", got, want)
	}
	if got := snap.Children("token"); !slices.Equal(got, []NodeID{"token-error"}) {
		t.Fatalf("token children %v", got)
	}
	appended := append(snap.Children("transcript"), "intruder")
	if len(appended) != 5 || slices.Contains(snap.Children("transcript"), "intruder") {
		t.Fatal("a caller changed the immutable snapshot")
	}
}

func TestSnapshotRejectsMalformedTrees(t *testing.T) {
	cases := map[string]func([]SemanticNode) ([]SemanticNode, NodeID){
		"second root":          func(n []SemanticNode) ([]SemanticNode, NodeID) { n[1].Parent = ""; return n, "" },
		"missing parent":       func(n []SemanticNode) ([]SemanticNode, NodeID) { n[1].Parent = "absent"; return n, "" },
		"child before parent":  func(n []SemanticNode) ([]SemanticNode, NodeID) { n[3], n[4] = n[4], n[3]; return n, "" },
		"focus outside a tree": func(n []SemanticNode) ([]SemanticNode, NodeID) { return n, "elsewhere" },
		"no nodes":             func([]SemanticNode) ([]SemanticNode, NodeID) { return nil, "" },
	}
	for name, breakTree := range cases {
		nodes, focus := breakTree(validTree())
		if _, err := NewSemanticSnapshot(1, focus, nodes); err == nil {
			t.Errorf("%s: the tree was accepted", name)
		}
	}
}

type recordingObserver struct {
	changes       [][2]uint64
	announcements []Announcement
}

func (o *recordingObserver) SnapshotChanged(previous, next *SemanticSnapshot) {
	var before uint64
	if previous != nil {
		before = previous.Revision()
	}
	o.changes = append(o.changes, [2]uint64{before, next.Revision()})
}

func (o *recordingObserver) Announced(n Announcement) { o.announcements = append(o.announcements, n) }

func TestAccessibilityTreeNotifiesObserversUntilStopped(t *testing.T) {
	tree := NewAccessibilityTree(4, nil)
	o := new(recordingObserver)
	stop := tree.Observe(o)
	first, _ := NewSemanticSnapshot(1, "", validTree())
	second, _ := NewSemanticSnapshot(2, "", validTree())
	tree.Publish(first)
	tree.Publish(second)
	tree.Announce(Announcement{Kind: AnnounceCompletion, Text: "Done"})
	stop()
	tree.Publish(first)
	if !slices.Equal(o.changes, [][2]uint64{{0, 1}, {1, 2}}) || len(o.announcements) != 1 {
		t.Fatalf("observer saw changes %v and %d announcements", o.changes, len(o.announcements))
	}
	if tree.Snapshot() != first {
		t.Fatal("Snapshot does not return the latest published tree")
	}
}

func TestAccessibilityRequestsAreBoundedAndRevalidated(t *testing.T) {
	var woken atomic.Int32
	tree := NewAccessibilityTree(1, func() { woken.Add(1) })
	press := AccessibilityIntent{Node: "send", Revision: 3, Action: ActionActivate}
	if err := tree.Request(press); !errors.Is(err, ErrElementUnavailable) {
		t.Fatalf("request before any tree: %v", err)
	}
	snap, _ := NewSemanticSnapshot(3, "", validTree())
	tree.Publish(snap)
	stale := press
	stale.Revision = 2
	if err := tree.Request(stale); !errors.Is(err, ErrElementUnavailable) {
		t.Fatalf("stale revision: %v", err)
	}
	if err := tree.Request(press); err != nil {
		t.Fatal(err)
	}
	if err := tree.Request(press); !errors.Is(err, ErrAccessibilityBusy) {
		t.Fatalf("full queue: %v, want ErrAccessibilityBusy without blocking", err)
	}
	if got := <-tree.Intents(); got != press || woken.Load() != 1 {
		t.Fatalf("queued %+v, woke the window %d times", got, woken.Load())
	}
}

func TestAccessibilityTextQueriesLeaveRoomForControls(t *testing.T) {
	tree := NewAccessibilityTree(2, nil)
	nodes := append(validTree(), SemanticNode{ID: "unloaded", Parent: "transcript", Role: RoleDocument, TextState: TextUncached})
	snapshot, err := NewSemanticSnapshot(1, "", nodes)
	if err != nil {
		t.Fatal(err)
	}
	tree.Publish(snapshot)
	for range 8 {
		if value, err := tree.QueryText("unloaded", 1); value != "" || !errors.Is(err, ErrAccessibilityBusy) {
			t.Fatalf("uncached query returned %q and %v", value, err)
		}
	}
	press := AccessibilityIntent{Node: "send", Revision: 1, Action: ActionActivate}
	if err := tree.Request(press); err != nil {
		t.Fatalf("repeated text queries blocked the control: %v", err)
	}
	if got := <-tree.Intents(); got.Action != ActionLoadText || got.Node != "unloaded" {
		t.Fatalf("first intent = %+v, want a load of the unloaded range", got)
	}
	if got := <-tree.Intents(); got != press {
		t.Fatalf("second intent = %+v, want the control press", got)
	}
}

func TestChoosePasteFollowsTheSpecifiedPriority(t *testing.T) {
	cases := []struct {
		available ClipboardFormats
		kind      PasteKind
		want      ClipboardFormats
	}{
		{ClipboardText | ClipboardImage, PasteOrdinary, ClipboardText},
		{ClipboardText | ClipboardImage, PasteImage, ClipboardImage},
		{ClipboardImage, PasteOrdinary, ClipboardImage},
		{ClipboardFiles | ClipboardText, PasteOrdinary, ClipboardFiles},
		{ClipboardText, PasteImage, ClipboardText},
	}
	for _, tc := range cases {
		got, err := ChoosePaste(tc.available, tc.kind)
		if err != nil || got != tc.want {
			t.Errorf("ChoosePaste(%b, %d) = %b, %v; want %b", tc.available, tc.kind, got, err, tc.want)
		}
	}
	if _, err := ChoosePaste(0, PasteOrdinary); !errors.Is(err, ErrUnsupportedPaste) {
		t.Fatalf("empty clipboard: %v", err)
	}
}
