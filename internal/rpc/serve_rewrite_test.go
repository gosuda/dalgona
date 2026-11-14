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

func TestRewriteAskApprovalSendsDiff(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	rewriteTools(t, r)
	writeWorkspaceFile(t, r, "a.go", "package a\n\nfunc Old() {}\n")
	r.llm.push(llmReply{body: sseCall("r", "rewrite", renameOldCall)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "rename")
	var request message
	select {
	case request = <-c.requests:
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
	}
	expect(t, request.Method == "item/fileChange/requestApproval", "approval request", request.Method)
	c.mu.Lock()
	order := slices.Clone(c.order)
	c.mu.Unlock()
	patch := slices.Index(order, "item/fileChange/patchUpdated")
	approval := slices.Index(order, "item/fileChange/requestApproval")
	expect(t, patch >= 0 && patch < approval, "patchUpdated before requestApproval", order)
	change := patchChanges(t, c)
	expect(t, len(change) == 1 && change[0].Additions != nil && *change[0].Additions == 1 && change[0].Deletions != nil && *change[0].Deletions == 1, "patchUpdated changes", change)
}

func TestRewriteCompletesAsFileChange(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	rewriteTools(t, r)
	writeWorkspaceFile(t, r, "a.go", "package a\n\nfunc Old() {}\n")
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
