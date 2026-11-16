package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	asktool "github.com/gosuda/dalgona/internal/dal/tool/ask"
	"github.com/gosuda/dalgona/internal/dal/tool/read"
	"github.com/gosuda/dalgona/internal/dal/tool/search"
	"github.com/gosuda/dalgona/internal/dal/tool/todo"
	"github.com/gosuda/dalgona/internal/dal/tool/webfetch"
)

func TestSpecSerialMarksOnlyStatefulReadTools(t *testing.T) {
	t.Parallel()
	cases := []struct {
		tool   tool.Tool
		name   string
		serial bool
	}{
		{tool: todo.New(), name: "todo", serial: true},
		{tool: asktool.New(nil), name: asktool.Name, serial: true},
		{tool: yieldTool{}, name: tool.YieldName, serial: true},
		{tool: read.New(), name: "read"},
		{tool: search.New(), name: "search"},
		{tool: webfetch.New(), name: "web_fetch"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			spec := tc.tool.Spec()
			if spec.Name != tc.name {
				t.Fatalf("tool name = %q, want %q", spec.Name, tc.name)
			}
			if class := tc.tool.Class(); class != tool.ClassRead {
				t.Fatalf("%s class = %d, want read class %d", tc.name, class, tool.ClassRead)
			}
			if spec.Serial != tc.serial {
				t.Fatalf("%s Serial = %t, want %t", tc.name, spec.Serial, tc.serial)
			}
		})
	}
}
