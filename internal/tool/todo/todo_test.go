package todo

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func TestTodoNormalizesPlanMetadata(t *testing.T) {
	t.Parallel()
	call := tool.Call{Args: []byte(`{"items":[{"text":"  Ship the release  ","status":"in_progress"},{"text":"Write notes","status":"pending"}]}`)}
	result, err := New().Run(t.Context(), call, tool.Env{})
	if err != nil {
		t.Fatal(err)
	}
	want := `{"items":[{"text":"Ship the release","status":"in_progress"},{"text":"Write notes","status":"pending"}]}`
	if !result.OK || string(result.Meta) != want || string(result.Output) != want {
		t.Fatalf("unexpected todo result: %+v", result)
	}
	if New().Class() != tool.ClassRead || New().Spec().Name != "todo" {
		t.Fatal("todo tool identity changed")
	}
}

func TestTodoRejectsBadStatus(t *testing.T) {
	t.Parallel()
	call := tool.Call{Args: []byte(`{"items":[{"text":"Ship","status":"started"}]}`)}
	_, err := New().Run(t.Context(), call, tool.Env{})
	if err == nil || !strings.Contains(err.Error(), "status must be one of pending, in_progress, done") {
		t.Fatalf("bad status error = %v", err)
	}
}

func TestTodoRejectsEmptyText(t *testing.T) {
	t.Parallel()
	call := tool.Call{Args: []byte(`{"items":[{"text":" \t\n ","status":"done"}]}`)}
	_, err := New().Run(t.Context(), call, tool.Env{})
	if err == nil || !strings.Contains(err.Error(), "text must not be empty") {
		t.Fatalf("empty text error = %v", err)
	}
}

func TestTodoAllowsEmptyPlan(t *testing.T) {
	t.Parallel()
	result, err := New().Run(t.Context(), tool.Call{Args: []byte(`{"items":[]}`)}, tool.Env{})
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || string(result.Meta) != `{"items":[]}` {
		t.Fatalf("unexpected empty plan: %+v", result)
	}
}
