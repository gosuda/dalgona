//go:build gui

package decision

import (
	"errors"
	"testing"

	"gioui.org/io/key"
	"gioui.org/io/semantic"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

func TestCommandCardShowsTheVerbatimCommandAndItsEndpoint(t *testing.T) {
	h := newHarness(t)
	h.frame(sceneWith(thread(
		request("r1", methodCommand, 1, commandParams),
		request("r2", methodCommand, 2, commandParams),
	)))
	h.must("Run this command?", semantic.Unknown)
	for _, want := range []string{
		`git  commit -m "fix: a  b" && rm -rf build`,
		"/work/repo",
		"commit the staged fix",
		"Approval required · 1 of 2 · on box",
	} {
		if !h.showsExactly(want) {
			t.Fatalf("the card does not show %q exactly; labels %q", want, h.labels())
		}
	}
	for _, fact := range []string{"same first two words", "thread journal"} {
		if !h.shows(fact) {
			t.Fatalf("Accept for thread explanation omits %q", fact)
		}
	}
}

func TestEscalatedCommandStatesItRunsOutsideTheSandbox(t *testing.T) {
	escalated := `{"threadId":"t1","turnId":"u1","itemId":"c2","startedAtMs":1,"command":"make install","cwd":"/work/repo","escalated":true}`
	for params, want := range map[string]bool{commandParams: false, escalated: true} {
		h := newHarness(t)
		h.frame(sceneWith(thread(request("r1", methodCommand, 1, params))))
		if got := h.shows("outside the harness sandbox"); got != want {
			t.Fatalf("escalation shown %v for %s, want %v", got, params, want)
		}
	}
}

func TestEachApprovalControlSendsItsWireDecision(t *testing.T) {
	for control, want := range map[string]string{
		"Accept": "accept", "Accept for thread": "acceptForSession", "Decline": "decline", "Cancel and interrupt": "cancel",
	} {
		h := newHarness(t)
		sc := sceneWith(thread(request("r1", methodCommand, 1, commandParams)))
		h.frame(sc)
		h.click(control, semantic.Button)
		h.frame(sc)
		got := h.onlyAnswer()
		if got.Request.Request != "r1" || got.Request.Thread != openThread || decisionOf(t, got.Result) != want {
			t.Fatalf("%s sent %+v %s, want r1 %s", control, got.Request, got.Result, want)
		}
	}
}

func TestShortcutsDecideWhileTheCardOwnsFocus(t *testing.T) {
	for name, want := range map[key.Name]string{"Y": "accept", "A": "acceptForSession", "N": "decline", key.NameEscape: "cancel"} {
		h := newHarness(t)
		sc := sceneWith(thread(request("r1", methodCommand, 1, commandParams)))
		h.frame(sc)
		h.frame(sc)
		h.press(name)
		h.frame(sc)
		if got := decisionOf(t, h.onlyAnswer().Result); got != want {
			t.Fatalf("%s decided %q, want %q", name, got, want)
		}
	}
}

func TestShortcutsStayWithOtherControlsWhenTheCardLacksFocus(t *testing.T) {
	h := newHarness(t)
	h.modal = true
	sc := sceneWith(thread(request("r1", methodCommand, 1, commandParams)))
	h.frame(sc)
	h.frame(sc)
	for _, name := range []key.Name{"Y", "A", "N", key.NameEscape} {
		h.press(name)
		h.frame(sc)
	}
	if len(h.submitted) != 0 {
		t.Fatalf("keys reached the card without its focus: %v", h.submitted)
	}
}

func TestOnlyOneDecisionLeavesTheCardWhileItsResponseIsPending(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(thread(request("r1", methodCommand, 1, commandParams)))
	h.frame(sc)
	h.click("Accept", semantic.Button)
	h.click("Decline", semantic.Button)
	h.press("N")
	h.frame(sc)
	h.frame(sc)
	h.press(key.NameEscape)
	h.frame(sc)
	if got := decisionOf(t, h.onlyAnswer().Result); got != "accept" {
		t.Fatalf("the single decision was %q, want the first click, accept", got)
	}
	if !h.shows("Decision sent") {
		t.Fatalf("a pending decision is not reported; labels %q", h.labels())
	}
}

func TestDecisionsStayDisabledUntilDetailsAreVerified(t *testing.T) {
	h := newHarness(t)
	loading := request("r1", methodCommand, 1, "")
	loading.Params, loading.Loading, loading.Actionable = nil, true, false
	sc := sceneWith(thread(loading))
	h.frame(sc)
	for _, control := range []string{"Accept", "Accept for thread", "Decline", "Cancel and interrupt"} {
		if n := h.must(control, semantic.Button); !n.Desc.Disabled {
			t.Fatalf("%s is enabled before the details are verified", control)
		}
	}
	h.click("Accept", semantic.Button)
	h.press("Y")
	h.frame(sc)
	if len(h.submitted) != 0 {
		t.Fatalf("a loading card submitted %v", h.submitted)
	}
}

func TestFailedDetailsKeepTheRequestAndOfferRetry(t *testing.T) {
	h := newHarness(t)
	failed := request("r1", methodCommand, 1, "")
	failed.Params, failed.Actionable, failed.Err = nil, false, errors.New("digest mismatch")
	sc := sceneWith(thread(failed))
	h.frame(sc)
	if !h.shows("digest mismatch") || !h.must("Accept", semantic.Button).Desc.Disabled {
		t.Fatalf("the failure is hidden or Accept is enabled; labels %q", h.labels())
	}
	h.click("Retry", semantic.Button)
	h.frame(sc)
	if len(h.submitted) != 1 || h.submitted[0] != (state.RetryDetails{Request: failed.Key}) {
		t.Fatalf("Retry submitted %v, want RetryDetails for r1", h.submitted)
	}
}

const fileParams = `{"threadId":"t1","turnId":"u1","itemId":"e1","startedAtMs":1,"reason":"update greeting"}`

const greetingDiff = "--- a/hello.txt\n+++ b/hello.txt\n@@ -1,2 +1,2 @@\n keep this line\n-old greeting\n+new  greeting\n"

func fileThread(changes []rpc.FileChange) *state.ThreadView {
	tv := thread(request("r1", methodFile, 1, fileParams))
	if changes != nil {
		tv.Turns = []state.TurnView{{ID: "u1", Items: []state.ItemView{{Key: state.ItemKey{Turn: "u1", Item: "e1"}, Changes: changes}}}}
	}
	return tv
}

func TestFileApprovalShowsTheSuppliedDiff(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(fileThread([]rpc.FileChange{{Path: "hello.txt", Kind: "update", Diff: greetingDiff, Additions: 1, Deletions: 1}}))
	h.frame(sc)
	h.must("Apply these changes?", semantic.Unknown)
	for _, want := range []string{"hello.txt", "old greeting", "new  greeting", "keep this line", "+1", "−1"} {
		if !h.showsExactly(want) {
			t.Fatalf("the diff omits %q; labels %q", want, h.labels())
		}
	}
	h.click("Accept", semantic.Button)
	h.frame(sc)
	if decisionOf(t, h.onlyAnswer().Result) != "accept" {
		t.Fatal("Accept did not accept a file change with its diff shown")
	}
}

func TestFileApprovalWithholdsAcceptUntilItsChangesArrive(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(fileThread(nil))
	h.frame(sc)
	for _, control := range []string{"Accept", "Accept for thread"} {
		if !h.must(control, semantic.Button).Desc.Disabled {
			t.Fatalf("%s is enabled while the changes are missing", control)
		}
	}
	h.click("Decline", semantic.Button)
	h.frame(sc)
	if decisionOf(t, h.onlyAnswer().Result) != "decline" {
		t.Fatal("Decline did not work while the changes were missing")
	}
}

const sandboxParams = `{"threadId":"t1","turnId":"u1","itemId":"c7","isBlocking":true,"questions":[{"id":"q1","header":"Sandbox","question":"The harness sandbox is not available: Landlock is not available on this kernel. How should commands in this thread run?","isOther":false,"isSecret":false,"multiSelect":false,"options":[{"label":"Accept","description":"Run without a sandbox. Every command asks for your approval first."},{"label":"Dangerously accept","description":"Run without a sandbox under approval mode auto."}]}]}`

func TestSandboxChoiceIsItsOwnCardWithDistinctChoices(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(thread(request("q", methodQuestion, 1, sandboxParams)))
	h.frame(sc)
	h.must("Run commands without a sandbox?", semantic.Unknown)
	if !h.shows("Landlock is not available on this kernel") || !h.shows("Sandbox choice required") {
		t.Fatalf("the sandbox card hides its cause; labels %q", h.labels())
	}
	if _, ok := h.node("Accept for thread", semantic.Button); ok {
		t.Fatal("the sandbox choice offers an ordinary thread grant")
	}
	h.click("Dangerously accept", semantic.Button)
	h.frame(sc)
	got := answersOf(t, h.onlyAnswer().Result)
	if len(got) != 1 || len(got["q1"]) != 1 || got["q1"][0] != "Dangerously accept" {
		t.Fatalf("Dangerously accept sent %v", got)
	}
}

func TestEscapeCancelsTheSandboxChoiceWithoutChoosing(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(thread(request("q", methodQuestion, 1, sandboxParams)))
	h.frame(sc)
	h.frame(sc)
	h.press(key.NameEscape)
	h.frame(sc)
	if got := answersOf(t, h.onlyAnswer().Result); len(got) != 0 {
		t.Fatalf("Escape sent %v, want an empty answers object", got)
	}
}

const permissionParams = `{"threadId":"t1","runnerId":"box-runner","bindingRevision":"9f2c","root":"/srv/app","rights":["read","terminal"],"reason":"open the file browser"}`

func TestWorkspacePermissionAllowsOnceAndDeclinesOnEscape(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(thread(request("w", methodPermission, 1, permissionParams)))
	h.frame(sc)
	h.must("Allow workspace access?", semantic.Unknown)
	for _, want := range []string{"box-runner", "9f2c", "/srv/app", "read, terminal", "open the file browser"} {
		if !h.showsExactly(want) {
			t.Fatalf("the permission card omits %q; labels %q", want, h.labels())
		}
	}
	for _, fact := range []string{"raw file contents", "commands can run", "until it expires or is revoked"} {
		if !h.shows(fact) {
			t.Fatalf("the permission card does not explain %q", fact)
		}
	}
	if _, ok := h.node("Accept for thread", semantic.Button); ok {
		t.Fatal("the permission card offers a thread grant")
	}
	h.press(key.NameEscape)
	h.frame(sc)
	if decisionOf(t, h.onlyAnswer().Result) != "decline" {
		t.Fatal("Escape did not decline the workspace grant")
	}
	h = newHarness(t)
	h.frame(sc)
	h.click("Allow once", semantic.Button)
	h.frame(sc)
	if decisionOf(t, h.onlyAnswer().Result) != "allowOnce" {
		t.Fatal("Allow once did not send allowOnce")
	}
}

const trustParams = `{"threadId":"t1","turnId":"u1","itemId":"","startedAtMs":1,"command":"docs: npx -y @acme/docs-mcp --port 0 TOKEN=<redacted>\nfs: /usr/bin/fs-mcp PATH=<redacted>","cwd":"/work/repo","reason":"Trust workspace /work/repo to start its MCP servers?"}`

func TestWorkspaceTrustShowsServersAndDefersWithNotNow(t *testing.T) {
	h := newHarness(t)
	sc := sceneWith(thread(request("m", methodCommand, 1, trustParams)))
	h.frame(sc)
	h.must("Trust this workspace?", semantic.Unknown)
	if !h.showsExactly("docs: npx -y @acme/docs-mcp --port 0 TOKEN=<redacted>\nfs: /usr/bin/fs-mcp PATH=<redacted>") || !h.showsExactly("/work/repo") {
		t.Fatalf("the trust card does not show the exact servers and workspace; labels %q", h.labels())
	}
	if _, ok := h.node("Accept for thread", semantic.Button); ok {
		t.Fatal("workspace trust offers a thread grant")
	}
	h.click("Not now", semantic.Button)
	h.frame(sc)
	if decisionOf(t, h.onlyAnswer().Result) != "cancel" {
		t.Fatal("Not now did not send cancel")
	}
}
