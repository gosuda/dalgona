//go:build gui

package decision

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dalgona/gui/access/accesstest"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func TestAccessibleApprovalNamesCommandWorkspaceReasonAndActions(t *testing.T) {
	h := newHarness(t)
	h.ax = accesstest.New()
	sc := sceneWith(thread(request("approval", methodCommand, 1, commandParams)))
	h.frame(sc)
	want := []struct {
		name, value string
		role        platform.Role
	}{
		{"Command", `git  commit -m "fix: a  b" && rm -rf build`, platform.RoleCode},
		{"Workspace", "/work/repo", platform.RoleDocument},
		{"Reason", "commit the staged fix", platform.RoleDocument},
	}
	for _, w := range want {
		if got := h.ax.Find(t, w.name, w.role); got.Value != w.value || got.Bounds.Empty() {
			t.Errorf("%s was not readable at its bounds: %+v", w.name, got)
		}
	}
	for _, label := range []string{"Accept", "Accept for thread", "Decline", "Cancel and interrupt"} {
		if n := h.ax.Find(t, label, platform.RoleButton); n.Bounds.Empty() || n.Action == "" {
			t.Errorf("missing action or bounds for %s: %+v", label, n)
		}
	}
}

func TestAccessibleFileApprovalExposesEachUnparsedDiff(t *testing.T) {
	h := newHarness(t)
	h.ax = accesstest.New()
	changes := []rpc.FileChange{
		{Path: "a.txt", Kind: "update", Diff: "@@ -1 +1 @@\n first without a file header\n"},
		{Path: "b.txt", Kind: "update", Diff: "@@ -1 +1 @@\n second without a file header\n"},
	}
	h.frame(sceneWith(fileThread(changes)))
	for _, change := range changes {
		if n := h.ax.Find(t, "Diff for "+change.Path, platform.RoleCode); n.Value != change.Diff {
			t.Errorf("diff for %s has value %q, want %q", change.Path, n.Value, change.Diff)
		}
	}
}

func TestAccessibleSecretAnswerNeverPublishesItsValue(t *testing.T) {
	h := newHarness(t)
	h.ax = accesstest.New()
	sc, _ := questionScene(strings.Replace(freeParams, `"isSecret":false`, `"isSecret":true`, 1))
	h.frame(sc)
	n := h.ax.Find(t, "Answer for Name", platform.RoleSecureField)
	const input = "only-typed-by-the-user"
	i := platform.AccessibilityIntent{Node: n.ID, Revision: h.ax.Tree.Snapshot().Revision(), Action: platform.ActionSetValue, Value: input}
	if err := h.ax.Tree.Request(i); err != nil {
		t.Fatal(err)
	}
	h.frame(sc)
	if got := h.dock.cards[sc.queue[0].view.Key].answers[0].text(); got != input {
		t.Fatalf("the secure field did not accept the replacement: length=%d", len(got))
	}
	secure := h.ax.Find(t, "Answer for Name", platform.RoleSecureField)
	if secure.Selection != (textinput.Range{}) || len(secure.Lines) != 0 {
		t.Fatal("the secure field published its text range or geometry")
	}
	for _, n := range h.ax.Nodes {
		if strings.Contains(n.Value+n.Name+n.Description+n.Placeholder+n.Reason, input) {
			t.Fatalf("node %q exposed a secure value", n.ID)
		}
	}
}
