package rpc

import (
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
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
