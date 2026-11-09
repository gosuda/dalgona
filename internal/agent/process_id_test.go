package agent

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/tool"
)

func TestPreparedProcessIDEscapesNestedCallID(t *testing.T) {
	classes := []tool.Class{
		tool.ClassRead,
		tool.ClassEdit,
		tool.ClassExec,
		tool.ClassAgent,
		tool.ClassScript,
		tool.ClassRemote,
	}
	for _, class := range classes {
		p := &prepared{call: event.ToolCall{ID: "run-1/3", Name: "search"}, class: class}
		id := p.processID()
		if filepath.Base(id) != id {
			t.Fatalf("class %v processID %q is not one path component", class, id)
		}
		if strings.Contains(id, "/") {
			t.Fatalf("class %v processID %q keeps a slash", class, id)
		}
	}
}
