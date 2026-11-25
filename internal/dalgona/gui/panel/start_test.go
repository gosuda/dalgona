//go:build gui

package panel

import (
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func newThreadHarness(t *testing.T, onScreen state.ThreadKey) *harness {
	t.Helper()
	h := newHarness(t, endpointOf(local, "local", client.PhaseConnected), endpointOf(box, "box", client.PhaseIdle))
	h.thread = onScreen
	h.open(shell.Route{Panel: "new", Thread: onScreen})
	return h
}

func TestRunOnListsEveryEndpointAndDefaultsToTheThreadOnScreen(t *testing.T) {
	h := newThreadHarness(t, threadB)
	if got := h.radios(); !slices.Equal(got, []string{"local Connected", "box Idle"}) {
		t.Fatalf("Run on choices %q", got)
	}
	if got := h.selected(); got != "box Idle" {
		t.Fatalf("Run on defaulted to %q, want the endpoint on screen", got)
	}
	if n := len(submitted[state.Connect](h.intents)); n != 0 {
		t.Fatal("opening the panel connected an endpoint")
	}
}

func TestRemoteStartSendsTheTypedPathThroughTheChosenEndpoint(t *testing.T) {
	h := newThreadHarness(t, threadB)
	h.typeInto(&h.panels.start.path, ` C:\work\repo `)
	h.click("Start thread")
	call := h.call("thread/start")
	if p := params[rpc.ThreadStartParams](t, call); call.Endpoint != box || !call.Operation || p.Cwd != ` C:\work\repo ` {
		t.Fatalf("thread/start on %s with %+v", call.Endpoint, p)
	}
	h.answer(call, rpc.ThreadStartResponse{Cwd: `C:\work\repo`, Thread: rpc.Thread{ID: "new-1"}})
	if len(h.navigated) != 1 || h.navigated[0] != (state.ThreadKey{Endpoint: box, Thread: "new-1"}) || h.closed != 1 {
		t.Fatalf("success opened %v", h.navigated)
	}
}

func TestLocalStartUsesTheNativeFolderChooser(t *testing.T) {
	h := newThreadHarness(t, threadA)
	h.click("Choose folder")
	if len(h.dialogs.requests) != 1 || h.dialogs.requests[0].Kind != platform.DialogChooseFolder {
		t.Fatalf("folder chooser requests %v", h.dialogs.requests)
	}
	h.panels.FolderChosen(platform.DialogResult{Files: []string{"/home/test/repo"}}, nil)
	h.settle()
	h.want("Working directory: /home/test/repo")
	h.click("Start thread")
	if p := params[rpc.ThreadStartParams](t, h.call("thread/start")); p.Cwd != "/home/test/repo" {
		t.Fatalf("local start cwd %q", p.Cwd)
	}
}

func TestStartFailureKeepsChoicesAndOffersRetry(t *testing.T) {
	h := newThreadHarness(t, threadB)
	h.typeInto(&h.panels.start.path, "/srv/app")
	h.click("Start thread")
	h.fail(h.call("thread/start"), "directory does not exist")
	h.want("Unable to start the thread on box: directory does not exist")
	if h.selected() != "box Idle" || h.panels.start.path.text() != "/srv/app" {
		t.Fatal("failure lost the endpoint or path")
	}
	h.click("Try again")
	if call := h.call("thread/start"); call.Endpoint != box {
		t.Fatalf("retry went to %s", call.Endpoint)
	}
}

func TestAdvisorStatesStayDistinctAndActionsRefresh(t *testing.T) {
	h := threadPanel(t, "advisor", "")
	h.want("Reading advisor status.")
	status := h.call("dalgona/advisor/status")
	h.answer(status, []rpc.AdvisorState{})
	h.want("No advisor is attached to this thread.")
	h.open(shell.Route{Panel: "advisor", Thread: threadA})
	h.answer(h.call("dalgona/advisor/status"), []rpc.AdvisorState{{Name: "critic", Model: "openai/main", Status: "reviewing", Notes: 2}})
	h.want("critic · openai/main", "Reviewing · 2 notes")
	h.click("Turn advisor off")
	set := h.call("dalgona/advisor/set")
	if p := params[rpc.DalgonaAdvisorSetParams](t, set); p.ThreadID != threadA.Thread || p.Enabled || !set.Operation {
		t.Fatalf("advisor set %+v", p)
	}
	h.take()
	h.answer(set, map[string]any{})
	h.call("dalgona/advisor/status")
	h.open(shell.Route{Panel: "advisor", Thread: threadA})
	h.fail(h.call("dalgona/advisor/status"), "journal unreadable")
	h.want("The advisor status could not be read: journal unreadable")
}

func TestReviewPreservesInstructionsExactly(t *testing.T) {
	h := threadPanel(t, "review", "")
	h.choose("Custom")
	h.typeInto(&h.panels.review.instructions, " \t ")
	h.click("Start review")
	h.want("Enter review instructions.")
	if slices.Contains(h.methods(), "review/start") {
		t.Fatal("whitespace-only instructions started a review")
	}
	h.typeInto(&h.panels.review.instructions, "check errors\n")
	h.click("Start review")
	call := h.call("review/start")
	p := params[rpc.ReviewStartParams](t, call)
	if p.ThreadID != threadA.Thread || p.Target.Type != reviewCustom || p.Target.Instructions != " \t check errors\n" || !call.Operation {
		t.Fatalf("review params %+v", p)
	}
}

func TestReviewValidatesBranchAndCommitWithoutRewriting(t *testing.T) {
	h := threadPanel(t, "review", "")
	h.choose("Base branch")
	h.typeInto(&h.panels.review.branch, "-bad")
	h.click("Start review")
	h.want("cannot start with a hyphen")
	if h.panels.review.branch.text() != "-bad" {
		t.Fatal("validation rewrote the branch")
	}
	h.choose("Commit")
	h.typeInto(&h.panels.review.commit, "abc1234")
	h.click("Start review")
	call := h.call("review/start")
	if p := params[rpc.ReviewStartParams](t, call); p.Target.Type != reviewCommit || p.Target.SHA != "abc1234" {
		t.Fatalf("commit review %+v", p.Target)
	}
	h.fail(call, "not a git repository")
	h.want("not a git repository")
	if h.panels.review.commit.text() != "abc1234" || h.closed != 0 {
		t.Fatal("review failure lost inputs or closed")
	}
}
