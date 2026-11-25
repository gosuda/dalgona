//go:build gui

package decision

import (
	"fmt"
	"image"
	"strings"
	"testing"

	"gioui.org/io/key"
	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

func TestChildRequestsJoinTheOpenThreadQueueInArrivalOrder(t *testing.T) {
	child := state.ThreadKey{Endpoint: openThread.Endpoint, Thread: "t1-child"}
	parent := thread(request("late", methodCommand, 9, commandParams))
	parent.Children = []state.Child{{Thread: child}}
	childView := &state.ThreadView{Key: child, Live: true, Link: liveLink, Thread: rpc.Thread{Name: "lint fixer"}}
	early := request("early", methodPermission, 4, permissionParams)
	early.Key.Thread = child
	childView.Requests = []state.RequestView{early}
	h := newHarness(t)
	h.frame(sceneWith(parent, childView))
	h.must("Allow workspace access?", semantic.Unknown)
	if !h.showsExactly("From subagent lint fixer") || !h.shows("· 1 of 2 · on box") {
		t.Fatalf("the child request is not first in the merged queue; labels %q", h.labels())
	}
	h.click("Allow once", semantic.Button)
	h.frame(sceneWith(parent, childView))
	if got := h.onlyAnswer().Request; got != early.Key {
		t.Fatalf("answered %+v, want the child request %+v", got, early.Key)
	}
}

func TestChildRequestListedUnderTheParentKeepsItsOwnKey(t *testing.T) {
	child := state.ThreadKey{Endpoint: openThread.Endpoint, Thread: "t1-child"}
	listed := request("c", methodCommand, 1, commandParams)
	listed.Key.Thread = child
	h := newHarness(t)
	sc := sceneWith(thread(listed))
	h.frame(sc)
	if !h.showsExactly("From subagent t1-child") {
		t.Fatalf("the subagent is not named; labels %q", h.labels())
	}
	h.click("Decline", semantic.Button)
	h.frame(sc)
	if got := h.onlyAnswer().Request; got != listed.Key {
		t.Fatalf("answered %+v, want %+v", got, listed.Key)
	}
}

func TestDisconnectedThreadShowsNoActionableCard(t *testing.T) {
	tv := thread(request("r1", methodCommand, 1, commandParams))
	tv.Live, tv.Offline = false, true
	h := newHarness(t)
	if dims := h.frame(sceneWith(tv)); dims.Size.Y != 0 || h.dock.Showing() {
		t.Fatalf("a disconnected thread still shows a card of height %d", dims.Size.Y)
	}
}

func TestArrivalTakesTitleFocusAndAnnouncesTheEndpoint(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(thread(request("r1", methodCommand, 1, commandParams)))
	h.frame(sc)
	h.frame(sc)
	if len(h.announced) != 1 || h.announced[0] != (platform.Announcement{Kind: platform.AnnounceDecision, Text: "Approval required on box"}) {
		t.Fatalf("arrival announced %v", h.announced)
	}
	if !h.dock.held {
		t.Fatal("the arriving card did not take focus")
	}
}

func TestResolutionAnnouncesItsOutcomeAndReleasesFocus(t *testing.T) {
	h := newHarness(t)
	rv := request("ask", methodQuestion, 1, freeParams)
	h.frame(sceneWith(thread(rv)))
	h.frame(sceneWith(thread(rv)))
	gone := thread()
	gone.Resolved = []state.ResolvedRequest{{Key: rv.Key, Method: methodQuestion, Item: "a1", Resolution: "timedOut"}}
	h.frame(sceneWith(gone))
	last := h.announced[len(h.announced)-1]
	if last != (platform.Announcement{Kind: platform.AnnounceCompletion, Text: "Question timed out on box"}) {
		t.Fatalf("resolution announced %v", h.announced)
	}
	if !h.dock.FocusReleased() || h.dock.FocusReleased() {
		t.Fatal("focus release must be reported exactly once after the focused card closes")
	}
	if _, kept := h.dock.cards[rv.Key]; kept {
		t.Fatal("a resolved request kept its card state")
	}
}

func numberedDiff(lines int) string {
	var diff strings.Builder
	diff.WriteString("--- a/big.txt\n+++ b/big.txt\n")
	fmt.Fprintf(&diff, "@@ -1,%d +1,%d @@\n", lines, lines)
	for i := range lines {
		fmt.Fprintf(&diff, " line %03d\n", i+1)
	}
	return diff.String()
}

func (h *harness) top(label string) (int, bool) {
	n, ok := h.node(label, semantic.Unknown)
	return n.Desc.Bounds.Min.Y, ok
}

func (h *harness) anchor(region image.Rectangle, last bool) (string, int) {
	h.t.Helper()
	name, at := "", 0
	for i := 1; i <= 200; i++ {
		label := fmt.Sprintf("line %03d", i)
		y, ok := h.top(label)
		if !ok || y < region.Min.Y || y+int(theme.DiffLineHeight) > region.Max.Y {
			continue
		}
		name, at = label, y
		if !last {
			break
		}
	}
	if name == "" {
		h.t.Fatalf("no diff row is fully inside the details region %v", region)
	}
	return name, at
}

func TestDetailsScrollByTheRegisteredSteps(t *testing.T) {
	h := newHarness(t)
	h.size = image.Pt(720, 600)
	sc := sceneWith(fileThread([]rpc.FileChange{{Path: "big.txt", Kind: "update", Diff: numberedDiff(200)}}))
	h.frame(sc)
	h.frame(sc)
	region := h.must(detailsLabel, semantic.Unknown).Desc.Bounds
	step := int(theme.RequestScrollStep)
	page := max(step, region.Dy()-step)
	if region.Dy() <= 2*step {
		t.Fatalf("the details region is %d tall, too small for this test", region.Dy())
	}
	h.scrollsBy(sc, region, []scrollCheck{{key.NameDownArrow, step}, {key.NamePageDown, page}, {key.NameUpArrow, -step}, {key.NamePageUp, -page}})
	h.press(key.NameEnd)
	h.frame(sc)
	if y, ok := h.top("line 200"); !ok || y+int(theme.DiffLineHeight) > region.Max.Y {
		t.Fatalf("End did not reach the end of the details: %+v", h.dock.details.Position)
	}
	h.press(key.NameHome)
	h.frame(sc)
	if y, ok := h.top("update greeting"); !ok || y < region.Min.Y || y > region.Min.Y+int(theme.Space4) {
		t.Fatalf("Home did not return to the first detail: %+v", h.dock.details.Position)
	}
}

type scrollCheck struct {
	key  key.Name
	want int
}

func (h *harness) scrollsBy(sc scene, region image.Rectangle, checks []scrollCheck) {
	h.t.Helper()
	for _, c := range checks {
		name, before := h.anchor(region, c.want > 0)
		h.press(c.key)
		h.frame(sc)
		if after, ok := h.top(name); !ok || before-after != c.want {
			h.t.Fatalf("%s moved %s by %d, want %d", c.key, name, before-after, c.want)
		}
	}
}

func TestShortDockKeepsHeaderAndActionsInsideItsHeight(t *testing.T) {
	h := newHarness(t)
	h.size = image.Pt(360, 520)
	sc := sceneWith(fileThread([]rpc.FileChange{{Path: "big.txt", Kind: "update", Diff: numberedDiff(120)}}))
	dims := h.frame(sc)
	if dims.Size.Y > h.size.Y {
		t.Fatalf("the card is %d tall in a %d dock", dims.Size.Y, h.size.Y)
	}
	for _, control := range []string{"Accept", "Accept for thread", "Decline", "Cancel and interrupt"} {
		if b := h.must(control, semantic.Button).Desc.Bounds; b.Max.Y > h.size.Y || b.Min.Y < 0 {
			t.Fatalf("%s lies outside the dock at %v", control, b)
		}
	}
	if b := h.must("Apply these changes?", semantic.Unknown).Desc.Bounds; b.Min.Y < 0 {
		t.Fatalf("the title scrolled away: %v", b)
	}
}

func TestUnsupportedRequestIsNamedWithoutBlockingTheQueue(t *testing.T) {
	h := newHarness(t)
	odd := request("x", "item/permissions/requestApproval", 1, `{}`)
	h.frame(sceneWith(thread(odd, request("r1", methodCommand, 2, commandParams))))
	h.must("Run this command?", semantic.Unknown)
	if !h.shows("cannot answer item/permissions/requestApproval on box") {
		t.Fatalf("the unsupported request is not named; labels %q", h.labels())
	}
}

func draftScene(i uint64) scene {
	key := state.ThreadKey{Endpoint: openThread.Endpoint, Thread: fmt.Sprintf("t%02d", i)}
	rv := request(fmt.Sprintf("ask%02d", i), methodQuestion, i+1, choiceParams)
	rv.Key.Thread = key
	tv := &state.ThreadView{Key: key, Live: true, Link: liveLink, Requests: []state.RequestView{rv}}
	return sceneWith(tv)
}

func TestUnsentAnswersPastTheLimitStayAndTheBlockedCardNamesTheOldest(t *testing.T) {
	h := newHarness(t)
	for i := range uint64(draftLimit) {
		sc := draftScene(i)
		h.frame(sc)
		h.click("fast", semantic.RadioButton)
		h.frame(sc)
	}
	blocked := draftScene(draftLimit)
	h.frame(blocked)
	if !h.must("fast", semantic.RadioButton).Desc.Disabled || !h.shows("The oldest is request ask00 in thread t00 on box") {
		t.Fatalf("the card past the limit does not stop drafting with a named notice; labels %q", h.labels())
	}
	h.frame(draftScene(0))
	if !h.must("fast", semantic.RadioButton).Desc.Selected {
		t.Fatal("the oldest unsent answer was dropped")
	}
}
