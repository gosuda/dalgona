//go:build unix

package patch

import (
	"context"
	"errors"
	"io/fs"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type regularStatRunner struct {
	runner.Runner
}

func (r regularStatRunner) Stat(ctx context.Context, path string) (runner.FileInfo, error) {
	info, err := r.Runner.Stat(ctx, path)
	info.Mode = 0o644
	return info, err
}

type fallbackCase struct {
	name  string
	leaf  string
	want  string
	fails bool
	miss  bool
}

var fallbackCases = []fallbackCase{
	{"IO-03/regular file", "regular.txt", "bytes\n", false, false},
	{"IO-04/absent leaf", "absent.txt", "", true, true},
	{"IO-04/named pipe", "pipe", "", true, false},
	{"IO-04/directory", "dir", "", true, false},
}

func TestReadWithoutAPinReadsOnlyRegularFiles(t *testing.T) {
	for _, test := range fallbackCases {
		t.Run(test.name, func(t *testing.T) {
			env, cwd, fifo := leafFixture(t)
			path := filepath.Join(cwd, "sub", test.leaf)
			var data []byte
			var err error
			returnsWithin(t, fifo, "readPinned", func() { data, err = readPinned(t.Context(), env, nil, path) })
			if (err != nil) != test.fails {
				t.Fatalf("read of %s error = %v, want failure %v", test.leaf, err, test.fails)
			}
			if errors.Is(err, fs.ErrNotExist) != test.miss {
				t.Fatalf("read of %s reports absence = %v, want %v: %v", test.leaf, !test.miss, test.miss, err)
			}
			if string(data) != test.want {
				t.Fatalf("read of %s = %q, want %q", test.leaf, data, test.want)
			}
		})
	}
}

func TestPathStateWithoutAPinIsUnknownForANonRegularLeaf(t *testing.T) {
	for _, leaf := range []string{"pipe", "dir"} {
		t.Run("IO-04 C13/"+leaf, func(t *testing.T) {
			env, cwd, fifo := leafFixture(t)
			empty := snapshot.Digest(nil)
			check := pathCheck{path: "sub/" + leaf, absolute: filepath.Join(cwd, "sub", leaf), before: empty, after: empty}
			var states []tool.PathState
			returnsWithin(t, fifo, "verifiedStates", func() { states = verifiedStates(t.Context(), env, []pathCheck{check}) })
			if len(states) != 1 || states[0].State != tool.PathUnknown {
				t.Fatalf("path states = %v, want one %q", states, tool.PathUnknown)
			}
		})
	}
}

func TestPlanReadRefusesALeafSwappedForAPipeAfterTheStat(t *testing.T) {
	env, _, fifo := leafFixture(t)
	env.Runner = regularStatRunner{env.Runner}
	var err error
	returnsWithin(t, fifo, "readTarget", func() { _, _, err = readTarget(t.Context(), env, fifo, "sub/pipe") })
	if err == nil {
		t.Fatal("readTarget accepted a named pipe as file content")
	}
	if errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("readTarget reported a named pipe as absent: %v", err)
	}
}
