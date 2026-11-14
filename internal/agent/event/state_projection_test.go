package event_test

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func TestFoldScriptEffectReportsMismatch(t *testing.T) {
	t.Parallel()
	_, err := foldEvents(t,
		event.TurnStarted{TurnID: "t1"},
		event.ScriptStarted{RunID: "r1", SourceBlob: event.BlobRef{Blob: digest, Bytes: 1}, Entry: "main", Args: jsontext.Value(`{}`), Seed: 9},
		event.ScriptEffect{RunID: "r1", N: 0, Op: "read", ArgsSHA: digest, Result: jsontext.Value(`1`)},
		event.ScriptEffect{RunID: "r1", N: 0, Op: "write", ArgsSHA: digest, Result: jsontext.Value(`1`)},
	)
	if err == nil {
		t.Fatal("fold accepted a changed script effect replay")
	}
	if err.Error() != "event: fold: script effect r1/0 mismatch" {
		t.Fatalf("mismatch error = %q", err.Error())
	}
}

func createdWithModel() event.SessionCreated {
	return event.SessionCreated{
		ID: "s", Cwd: "/w", Model: "base/m",
		Approval: "auto", Sandbox: "off", Runner: "local",
	}
}

func twoTurns(t *testing.T) event.State {
	t.Helper()
	state, err := foldEvents(t,
		createdWithModel(),
		event.TurnStarted{TurnID: "t1"},
		event.UserMessage{ItemID: "m1", TurnID: "t1", Kind: "prompt", Text: "first prompt"},
		event.TurnCompleted{TurnID: "t1", Status: "completed"},
		event.TurnStarted{TurnID: "t2"},
		event.UserMessage{ItemID: "m2", TurnID: "t2", Kind: "prompt", Text: "second", Model: "other/m"},
		event.TurnCompleted{TurnID: "t2", Status: "completed"},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	return state
}

func TestFoldOwnsSessionIdentity(t *testing.T) {
	t.Parallel()
	state := twoTurns(t)
	if state.Cwd != "/w" || state.SessionModel != "base/m" {
		t.Fatalf("session identity = %+v", state)
	}
	if state.Model != "other/m" {
		t.Fatalf("model = %q, want other/m", state.Model)
	}
	if state.Preview != "first prompt" {
		t.Fatalf("preview = %q", state.Preview)
	}
}

func TestFoldBranchRewindsModel(t *testing.T) {
	t.Parallel()
	state, err := event.Fold(twoTurns(t), 8, event.Branch{ParentSeq: 4})
	if err != nil {
		t.Fatalf("fold branch: %v", err)
	}
	if state.Model != "base/m" {
		t.Fatalf("model after rewind = %q, want base/m", state.Model)
	}
	if state.Preview != "first prompt" {
		t.Fatalf("preview moved to %q", state.Preview)
	}
}

func TestFoldRootBranchKeepsCreatedModel(t *testing.T) {
	t.Parallel()
	state, err := event.Fold(twoTurns(t), 8, event.Branch{ParentSeq: 0})
	if err != nil {
		t.Fatalf("fold root branch: %v", err)
	}
	if state.Model != "base/m" {
		t.Fatalf("model after root branch = %q, want base/m", state.Model)
	}
}

func TestFoldModelReturnsToLeafBranch(t *testing.T) {
	t.Parallel()
	state, err := foldEvents(t,
		createdWithModel(),
		event.TurnStarted{TurnID: "t1"},
		event.UserMessage{ItemID: "m1", TurnID: "t1", Kind: "prompt", Text: "one", Model: "a/m"},
		event.TurnCompleted{TurnID: "t1", Status: "completed"},
		event.TurnStarted{TurnID: "t2"},
		event.UserMessage{ItemID: "m2", TurnID: "t2", Kind: "prompt", Text: "two", Model: "b/m"},
		event.TurnCompleted{TurnID: "t2", Status: "completed"},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	reverted, err := event.Fold(state, 8, event.Branch{ParentSeq: 4})
	if err != nil {
		t.Fatalf("fold revert: %v", err)
	}
	if reverted.Model != "a/m" {
		t.Fatalf("model after revert = %q, want a/m", reverted.Model)
	}
	leaf, err := event.Fold(reverted, 9, event.Branch{ParentSeq: 7})
	if err != nil {
		t.Fatalf("fold leaf: %v", err)
	}
	if leaf.Model != "b/m" {
		t.Fatalf("model after returning to leaf = %q, want b/m", leaf.Model)
	}
	if state.Model != "b/m" {
		t.Fatalf("revert mutated its input model = %q", state.Model)
	}
}

func TestFoldPreviewKeepsBlobRefForHost(t *testing.T) {
	t.Parallel()
	blob := &event.BlobRef{Blob: digest, Bytes: 8}
	state, err := foldEvents(t,
		createdWithModel(),
		event.TurnStarted{TurnID: "t1"},
		event.UserMessage{ItemID: "m1", TurnID: "t1", Kind: "prompt", TextBlob: blob},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	if state.Preview != "" || state.PreviewBlob != blob {
		t.Fatalf("preview = %q blob = %+v", state.Preview, state.PreviewBlob)
	}
}

func TestPreviewFallsThroughToLaterPrompt(t *testing.T) {
	t.Parallel()
	pending := &event.BlobRef{Blob: digest, Bytes: 8}
	state, err := foldEvents(t,
		createdWithModel(),
		event.TurnStarted{TurnID: "t1"},
		event.UserMessage{ItemID: "m1", TurnID: "t1", Kind: "prompt", TextBlob: pending},
		event.UserMessage{ItemID: "m2", TurnID: "t1", Kind: "prompt", Text: "second"},
	)
	if err != nil {
		t.Fatalf("fold: %v", err)
	}
	if state.Preview != "second" {
		t.Fatalf("preview = %q, want the first prompt that produced text", state.Preview)
	}
	if state.PreviewBlob != pending {
		t.Fatal("fold dropped the earlier blob the host must try first")
	}
}
