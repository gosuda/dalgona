package rpcserver

import (
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestEditApprovalSendsTheDiffFirst(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	editStyleTools(t, r)
	writeWorkspaceFile(t, r, "a.txt", "one\ntwo\n")
	r.llm.push(llmReply{body: sseCall("p", "patch", `{"changes":[{"path":"a.txt","old":"one","new":"uno"}]}`)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "edit")
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
	expect(t, len(change) == 1 && change[0].Additions != nil && *change[0].Additions == 1, "patchUpdated changes", change)
	applied := applyUnifiedWithGit(t, change[0].Diff, "one\ntwo\n")
	c.respond(request.ID, `{"decision":"accept"}`)
	c.await("turn/completed", nil)
	post, err := os.ReadFile(filepath.Join(r.cwd, "a.txt"))
	if err != nil {
		t.Fatal(err)
	}
	if applied != string(post) {
		t.Fatalf("patchUpdated diff applied to the pre-image gives %q, want the approved change %q", applied, post)
	}
}

func TestDeclinedEditLeavesDiskUnchanged(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	editStyleTools(t, r)
	writeWorkspaceFile(t, r, "a.txt", "one\ntwo\n")
	r.llm.push(llmReply{body: sseCall("p", "patch", `{"changes":[{"path":"a.txt","old":"one","new":"uno"}]}`)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "edit")
	select {
	case request := <-c.requests:
		c.respond(request.ID, `{"decision":"decline"}`)
	case <-time.After(waitLimit):
		t.Fatal("no approval request")
	}
	c.await("turn/completed", nil)
	if body := readWorkspace(t, r, "a.txt"); body != "one\ntwo\n" {
		t.Fatalf("declined edit changed a.txt to %q", body)
	}
}

func TestReadOnlyCallEmitsNoPatchUpdated(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	editStyleTools(t, r)
	writeWorkspaceFile(t, r, "a.txt", "one\ntwo\n")
	r.llm.push(llmReply{body: sseCall("rd", "read", `{"path":"a.txt"}`)}, llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "read")
	log := readTurn(t, c.collect())
	expect(t, log.status == rpc.StatusCompleted, "turn status", log.status)
	c.mu.Lock()
	methods := slices.Clone(c.order)
	c.mu.Unlock()
	expect(t, !slices.Contains(methods, "item/fileChange/patchUpdated"), "read-only turn emitted patchUpdated", methods)
}

func patchChanges(t *testing.T, c *wireClient) []wireChange {
	t.Helper()
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, m := range c.arrivals {
		if m.Method != "item/fileChange/patchUpdated" {
			continue
		}
		var note struct {
			Changes []wireChange `json:"changes"`
		}
		mustUnmarshal(t, m.Params, &note)
		return note.Changes
	}
	t.Fatal("no item/fileChange/patchUpdated notification")
	return nil
}
