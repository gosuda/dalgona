package rpc

import (
	"context"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/read"
	"github.com/gosuda/dalgona/internal/tool/rewrite"
	"github.com/gosuda/dalgona/internal/tool/search"
)

const renameOldCall = `{"lang":"go","pattern":"((identifier) @name (#eq? @name \"Old\"))","target":"name","replace":"New"}`

func rewriteTools(t *testing.T, r *rig) {
	t.Helper()
	r.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
		return []tool.Tool{read.New(), search.New(), rewrite.New()}, nil
	}
	reopen(t, r)
}

const (
	renameBefore = "package a\n\nfunc Old() {}\n"
	renameAfter  = "package a\n\nfunc New() {}\n"
	otherBefore  = "func Old() {}\n"
)

func awaitRewriteApproval(t *testing.T) (*rig, *wireClient, message) {
	t.Helper()
	r := newRig(t, config.ApprovalAsk)
	rewriteTools(t, r)
	writeWorkspaceFile(t, r, "a.go", renameBefore)
	writeWorkspaceFile(t, r, "notes.txt", otherBefore)
	r.llm.push(llmReply{body: sseCall("r", "rewrite", renameOldCall)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "rename")
	select {
	case request := <-c.requests:
		return r, c, request
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
	}
	return nil, nil, message{}
}

func TestRewriteAskApprovalSendsDiff(t *testing.T) {
	r, c, request := awaitRewriteApproval(t)
	expect(t, request.Method == "item/fileChange/requestApproval", "approval request", request.Method)
	c.mu.Lock()
	order := slices.Clone(c.order)
	c.mu.Unlock()
	patch := slices.Index(order, "item/fileChange/patchUpdated")
	approval := slices.Index(order, "item/fileChange/requestApproval")
	expect(t, patch >= 0 && patch < approval, "patchUpdated before requestApproval", order)
	change := patchChanges(t, c)
	expect(t, len(change) == 1 && change[0].Additions != nil && *change[0].Additions == 1 && change[0].Deletions != nil && *change[0].Deletions == 1, "patchUpdated changes", change)
	expect(t, readWorkspace(t, r, "a.go") == renameBefore, "a.go before the answer", readWorkspace(t, r, "a.go"))
	c.respond(request.ID, `{"decision":"accept"}`)
	c.await("turn/completed", nil)
	expect(t, readWorkspace(t, r, "a.go") == renameAfter, "a.go after accept", readWorkspace(t, r, "a.go"))
	expect(t, readWorkspace(t, r, "notes.txt") == otherBefore, "unselected notes.txt after accept", readWorkspace(t, r, "notes.txt"))
}

func TestDeclinedRewriteLeavesDiskUnchanged(t *testing.T) {
	r, c, request := awaitRewriteApproval(t)
	c.respond(request.ID, `{"decision":"decline"}`)
	c.await("turn/completed", nil)
	expect(t, readWorkspace(t, r, "a.go") == renameBefore, "a.go after decline", readWorkspace(t, r, "a.go"))
	expect(t, readWorkspace(t, r, "notes.txt") == otherBefore, "unselected notes.txt after decline", readWorkspace(t, r, "notes.txt"))
}

func TestRewriteCompletesAsFileChange(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	rewriteTools(t, r)
	writeWorkspaceFile(t, r, "a.go", renameBefore)
	writeWorkspaceFile(t, r, "notes.txt", otherBefore)
	r.llm.push(llmReply{body: sseCall("r", "rewrite", renameOldCall)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "rename")
	live := liveItem(t, c.collect(), "r")
	expect(t, live.Type == "fileChange" && len(live.Changes) == 1, "live item", live)
	requireRenameChange(t, live.Changes[0])
	resumed := resumedItem(t, r, threadID, "r")
	if !reflect.DeepEqual(live.Changes, resumed.Changes) {
		t.Fatalf("live changes %+v differ from resumed %+v", live.Changes, resumed.Changes)
	}
	expect(t, readWorkspace(t, r, "a.go") == renameAfter, "a.go after the rewrite", readWorkspace(t, r, "a.go"))
	expect(t, readWorkspace(t, r, "notes.txt") == otherBefore, "unselected notes.txt after the rewrite", readWorkspace(t, r, "notes.txt"))
}

func requireRenameChange(t *testing.T, change wireChange) {
	t.Helper()
	if change.Kind.Type != "update" || change.Additions == nil || *change.Additions != 1 || change.Deletions == nil || *change.Deletions != 1 {
		t.Fatalf("change = %+v, want update with 1 addition and 1 deletion", change)
	}
	for _, line := range []string{"-func Old() {}", "+func New() {}"} {
		if !strings.Contains(change.Diff, line+"\n") {
			t.Fatalf("diff lacks %q:\n%s", line, change.Diff)
		}
	}
}
