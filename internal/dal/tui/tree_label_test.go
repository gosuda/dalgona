package tui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func treeWithPreview(preview string) *rpc.ThreadTreeResponse {
	return &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{
		{TurnID: "turn-1", Preview: "earlier prompt", Active: true},
		{TurnID: "turn-2", Preview: preview, Active: true},
	}}
}

func openTreeList(t *testing.T, op string, tree *rpc.ThreadTreeResponse) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 30})
	m.thread.id = "t1"
	m.thread.state = stateIdle
	m, _ = drive(t, m, resultMsg{op: op, seq: m.listSeq, value: tree})
	if m.list == nil {
		t.Fatalf("%s opened no list", op)
	}
	return m
}

func TestTreeListShowsMultilinePreviewOnOneRow(t *testing.T) {
	t.Parallel()
	for _, op := range []string{"tree", "revertTree", "forkTree", "rewindTree"} {
		t.Run(op, func(t *testing.T) {
			t.Parallel()
			single := openTreeList(t, op, treeWithPreview("first line second line"))
			multi := openTreeList(t, op, treeWithPreview("first line\nsecond line"))
			if got, want := strings.Count(multi.View().Content, "\n"), strings.Count(single.View().Content, "\n"); got != want {
				t.Fatalf("dock has %d rows with a two-line preview, want %d:\n%s", got, want, multi.View().Content)
			}
			content := multi.View().Content
			assertContains(t, content, "first line second line", "earlier prompt")
			assertFrame(t, strings.Split(content, "\n"), 100, 30)
		})
	}
}
