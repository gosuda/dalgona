package todo

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

func TestTodoInvalidItemsStayOrdinary(t *testing.T) {
	t.Parallel()
	result, err := New().Run(t.Context(), tool.Call{Args: []byte(`{"items":[{"text":"Ship","status":"started"}]}`)}, tool.Env{})
	if err == nil {
		t.Fatal("an invalid status was accepted")
	}
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure = %q, want invalid arguments to stay ordinary", result.Failure)
	}
}
