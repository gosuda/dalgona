package rpc

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/textdiff"
)

type wireChange struct {
	Path string `json:"path"`
	Kind struct {
		Type string `json:"type"`
	} `json:"kind"`
	Diff        string `json:"diff"`
	Additions   *int   `json:"additions"`
	Deletions   *int   `json:"deletions"`
	DiffOmitted *bool  `json:"diffOmitted"`
}

type wireItem struct {
	ID      string       `json:"id"`
	Type    string       `json:"type"`
	Changes []wireChange `json:"changes"`
}

type wireResume struct {
	Thread struct {
		Turns []struct {
			Items []wireItem `json:"items"`
		} `json:"turns"`
	} `json:"thread"`
}

func resumedItem(t *testing.T, r *rig, threadID, itemID string) wireItem {
	t.Helper()
	var resumed wireResume
	r.connect().ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	for _, turn := range resumed.Thread.Turns {
		for _, item := range turn.Items {
			if item.ID == itemID {
				return item
			}
		}
	}
	t.Fatalf("thread/resume has no item %s", itemID)
	return wireItem{}
}

func writeWorkspaceFile(t *testing.T, r *rig, name, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(r.cwd, name), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

func requireUnifiedChange(t *testing.T, change wireChange) {
	t.Helper()
	if change.Kind.Type != "update" || change.Additions == nil || *change.Additions != 1 || change.Deletions == nil || *change.Deletions != 1 {
		t.Fatalf("change = %+v, want update with 1 addition and 1 deletion", change)
	}
	for _, line := range []string{"--- a/a.txt", "+++ b/a.txt", "-one", "+uno"} {
		if !strings.Contains(change.Diff, line+"\n") {
			t.Fatalf("diff lacks %q:\n%s", line, change.Diff)
		}
	}
}

func requireOmittedChange(t *testing.T, change wireChange) {
	t.Helper()
	if change.DiffOmitted == nil || !*change.DiffOmitted || change.Diff != "" || change.Additions != nil || change.Deletions != nil {
		t.Fatalf("omitted change = %+v, want diffOmitted true, empty diff, no counts", change)
	}
}

func requireNormalChange(t *testing.T, change wireChange) {
	t.Helper()
	if change.DiffOmitted != nil || change.Additions == nil || change.Deletions == nil {
		t.Fatalf("normal change = %+v, want no diffOmitted member and both counts", change)
	}
}

func TestFileChangeItemCarriesUnifiedDiff(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	editStyleTools(t, r)
	writeWorkspaceFile(t, r, "a.txt", "one\ntwo\n")
	r.llm.push(llmReply{body: sseCall("p", "patch", `{"changes":[{"path":"a.txt","old":"one","new":"uno"}]}`)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "edit")
	live := liveItem(t, c.collect("turn/completed"), "p")
	requireFileChangeItem(t, live)
	resumed := resumedItem(t, r, threadID, "p")
	requireFileChangeItem(t, resumed)
	if !reflect.DeepEqual(live.Changes, resumed.Changes) {
		t.Fatalf("live changes %+v differ from resumed %+v", live.Changes, resumed.Changes)
	}
}

func requireFileChangeItem(t *testing.T, item wireItem) {
	t.Helper()
	if item.Type != "fileChange" || len(item.Changes) != 1 {
		t.Fatalf("item = %+v, want one fileChange", item)
	}
	requireUnifiedChange(t, item.Changes[0])
}

func liveItem(t *testing.T, msgs []message, itemID string) wireItem {
	t.Helper()
	for _, m := range msgs {
		if m.Method != "item/completed" {
			continue
		}
		var note struct {
			Item wireItem `json:"item"`
		}
		mustUnmarshal(t, m.Params, &note)
		if note.Item.ID == itemID {
			return note.Item
		}
	}
	t.Fatalf("no live item/completed for %s", itemID)
	return wireItem{}
}

func TestDiffOmittedOnTheWire(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	editStyleTools(t, r)
	writeWorkspaceFile(t, r, "big.txt", strings.Repeat("a", textdiff.Limit)+"z")
	writeWorkspaceFile(t, r, "a.txt", "one\ntwo\n")
	r.llm.push(llmReply{body: sseCall("big", "patch", `{"changes":[{"path":"big.txt","old":"z","new":"y"}]}`)})
	r.llm.push(llmReply{body: sseText("done")})
	r.llm.push(llmReply{body: sseCall("small", "patch", `{"changes":[{"path":"a.txt","old":"one","new":"uno"}]}`)})
	r.llm.push(llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	startTurn(t, c, threadID, "big")
	bigMsgs := c.collect("turn/completed")
	startTurn(t, c, threadID, "small")
	smallMsgs := c.collect("turn/completed")

	requireOmittedChange(t, liveItem(t, bigMsgs, "big").Changes[0])
	requireNormalChange(t, liveItem(t, smallMsgs, "small").Changes[0])
	requireOmittedChange(t, resumedItem(t, r, threadID, "big").Changes[0])
	requireNormalChange(t, resumedItem(t, r, threadID, "small").Changes[0])
}
