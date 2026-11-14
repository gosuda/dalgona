package rpc

import (
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/patch"
)

func TestChildEditApprovalSentOncePerConnection(t *testing.T) {
	r := newDalgonaRigWithTools(t, exec.New(), patch.New("replace"))
	writeWorkspaceFile(t, r, "a.txt", "one\ntwo\n")
	r.llm.push(
		llmReply{body: sseCall("call-task", "task", `{"prompt":"run it"}`)},
		llmReply{body: sseCall("call-first", "exec", `{"command":"echo first"}`)},
		llmReply{body: sseCall("call-patch", "patch", `{"changes":[{"path":"a.txt","old":"one","new":"uno"}]}`)},
		llmReply{body: sseCall("call-yield", "yield", `{"data":"ok"}`)},
		llmReply{body: sseText("parent done")},
	)
	c := r.connect()
	parentID := startThread(t, r, c, nil)
	startTurn(t, c, parentID, "delegate")
	first := awaitFirstApproval(t, c)
	childID := fieldOf(t, first.Params, "threadId")
	if childID == "" || childID == parentID {
		t.Fatalf("child thread id %q", childID)
	}
	var resumed ThreadResumeResponse
	c.ok("thread/resume", map[string]any{"threadId": childID}, &resumed)
	c.respond(first.ID, `{"decision":"accept"}`)
	second := awaitItemApproval(t, c, "call-patch")
	if duplicates := countDuplicateDeliveries(t, c, second.ID, 2*time.Second); duplicates != 0 {
		t.Fatalf("duplicate approval request %s delivered %d extra times", second.ID, duplicates)
	}
	if sent := countPatchUpdated(t, c, "call-patch"); sent != 1 {
		t.Fatalf("item/fileChange/patchUpdated for call-patch sent %d times, want 1", sent)
	}
	c.respond(second.ID, `{"decision":"accept"}`)
	log := readTurn(t, c.collect())
	expect(t, log.status == statusCompleted, "parent turn status", log.status)
}

func countPatchUpdated(t *testing.T, c *wireClient, itemID string) int {
	t.Helper()
	c.mu.Lock()
	defer c.mu.Unlock()
	count := 0
	for _, m := range c.arrivals {
		if m.Method == "item/fileChange/patchUpdated" && fieldOf(t, m.Params, "itemId") == itemID {
			count++
		}
	}
	return count
}
