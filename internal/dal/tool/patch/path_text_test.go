package patch

import (
	"fmt"
	"os"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const (
	pathTextBytes = 16 * 1024
	pathTextError = "patch: the paths of this call total over 16 KiB; split the change into several calls"
	pathDirectory = 200
)

func pathDir() string { return strings.Repeat("d", pathDirectory) }

func spreadPaths(count, total int) []string {
	dir := pathDir()
	fileBytes := total - count*(len(dir)+1)
	paths := make([]string, count)
	for i := range paths {
		size := fileBytes / count
		if i < fileBytes%count {
			size++
		}
		paths[i] = dir + "/" + fmt.Sprintf("%02d", i) + strings.Repeat("x", size-2)
	}
	return paths
}

func createSections(paths []string) string {
	var out strings.Builder
	for _, path := range paths {
		out.WriteString("[" + path + "#NEW]\nPUT >$:\n+x\n")
	}
	return out.String()
}

type pathCall struct {
	env     tool.Env
	cwd     string
	input   string
	entries int
}

func createCall(t *testing.T, count, total int) pathCall {
	t.Helper()
	env, cwd := editTestEnv(t)
	return pathCall{env: env, cwd: cwd, input: createSections(spreadPaths(count, total))}
}

func existingCall(t *testing.T, count, total int, operations func(path, ref string) string) pathCall {
	t.Helper()
	env, cwd := editTestEnv(t)
	paths := spreadPaths(count, total)
	writeFixture(t, cwd, paths[0], "one\ntwo\n")
	ref := seedReference(env, paths[0], "one\ntwo\n", true)
	return pathCall{env: env, cwd: cwd, input: operations(paths[0], ref) + createSections(paths[1:]), entries: 1}
}

func (c pathCall) plan(t *testing.T, style Style) (tool.Plan, error) {
	t.Helper()
	args := []byte(`{"input":` + snapshot.JSONString(c.input) + `}`)
	return New(string(style)).Plan(t.Context(), tool.Call{ID: "call", Args: args}, boundEvidence(c.env, c.input))
}

func (c pathCall) requireRefused(t *testing.T, style Style) {
	t.Helper()
	plan, err := c.plan(t, style)
	if err == nil {
		plan.Discard()
		t.Fatalf("%s planned a call whose paths total over 16 KiB", style)
	}
	if err.Error() != pathTextError {
		t.Fatalf("%s error %q, want %q", style, err, pathTextError)
	}
	requireWrappedOutcome(t, err, style, rejected(tool.ReasonResourceLimit, tool.ErrClassLimit))
	found, readErr := os.ReadDir(c.cwd)
	if readErr != nil || len(found) != c.entries {
		t.Fatalf("%s left %v in the workspace, want %d entries: %v", style, found, c.entries, readErr)
	}
}

func (c pathCall) requirePlanned(t *testing.T, style Style, files int) {
	t.Helper()
	plan, err := c.plan(t, style)
	if err != nil {
		t.Fatalf("%s refused a call whose paths total at most 16 KiB: %v", style, err)
	}
	defer plan.Discard()
	if len(plan.Files) != files {
		t.Fatalf("%s plan holds %d files, want %d", style, len(plan.Files), files)
	}
}

func TestPathTextOver16KiBIsResourceLimit(t *testing.T) {
	t.Run("C14/over", func(t *testing.T) {
		for _, style := range profileStyles {
			createCall(t, 64, pathTextBytes+1).requireRefused(t, style)
		}
	})
	t.Run("C14/exact", func(t *testing.T) {
		for _, style := range profileStyles {
			createCall(t, 64, pathTextBytes).requirePlanned(t, style, 64)
		}
	})
	t.Run("C14/duplicates", func(t *testing.T) {
		twice := func(path, ref string) string {
			return profileInput(path, ref, "PUT 1:\n+a\nPUT 2:\n+b\n")
		}
		for _, style := range profileStyles {
			existingCall(t, 63, pathTextBytes, twice).requirePlanned(t, style, 63)
		}
	})
	t.Run("C14/rename", func(t *testing.T) {
		destination := pathDir() + "/moved"
		move := func(path, ref string) string {
			return profileInput(path, ref, "MV "+destination+"\n")
		}
		for _, style := range profileStyles {
			existingCall(t, 64, pathTextBytes+1-len(destination), move).requireRefused(t, style)
			existingCall(t, 64, pathTextBytes-len(destination), move).requirePlanned(t, style, 65)
		}
	})
}

func TestLegacyStylesTakeNoPathTextCap(t *testing.T) {
	paths := spreadPaths(64, pathTextBytes+1)
	var changes []string
	for _, path := range paths {
		changes = append(changes, `{"path":`+snapshot.JSONString(path)+`,"create":"x\n"}`)
	}
	calls := []struct {
		style Style
		args  string
	}{
		{Replace, `{"changes":[` + strings.Join(changes, ",") + `]}`},
		{Hashline, `{"input":` + snapshot.JSONString(createSections(paths)) + `}`},
	}
	for _, item := range calls {
		t.Run("COMPAT-01/"+string(item.style), func(t *testing.T) {
			env, _ := editTestEnv(t)
			plan, err := New(string(item.style)).Plan(t.Context(), tool.Call{ID: "call", Args: []byte(item.args)}, env)
			if err != nil {
				t.Fatalf("legacy %s refused 16385 bytes of paths: %v", item.style, err)
			}
			defer plan.Discard()
			if len(plan.Files) != 64 {
				t.Fatalf("legacy %s plan holds %d files, want 64", item.style, len(plan.Files))
			}
		})
	}
}
