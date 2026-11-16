package patch

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func requireFileGone(t *testing.T, cwd string) {
	t.Helper()
	if _, err := os.Stat(filepath.Join(cwd, "f.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("REM left the file in place: %v", err)
	}
}

func requireCoverageRefusal(t *testing.T, env tool.Env, cwd, ref, source string) {
	t.Helper()
	_, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 5:\n+FIVE\n"))
	if err == nil || !strings.Contains(err.Error(), "need prior observation") {
		t.Fatalf("enhanced edit of an unshown line: error = %v, want a prior-observation refusal", err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != source {
		t.Fatalf("refused edit changed the fixture to %q", actual)
	}
}

func TestRemIsEligibleWithPartialShownRanges(t *testing.T) {
	const source = "one\ntwo\nthree\nfour\nfive\n"
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		t.Run(string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "f.txt", source)
			ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 2}})
			if style == HashlineEnhanced {
				requireCoverageRefusal(t, env, cwd, ref, source)
			}
			if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "REM")); err != nil {
				t.Fatalf("REM with lines 1-2 of 5 shown: %v", err)
			}
			requireFileGone(t, cwd)
		})
	}
}

func TestRemRefusesChangedSourceDespitePartialShownRanges(t *testing.T) {
	const source = "one\ntwo\nthree\n"
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		t.Run(string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "f.txt", source)
			ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 1}})
			writeFixture(t, cwd, "f.txt", "one\ntwo\nthree\nextra\n")
			if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "REM")); err == nil {
				t.Fatal("REM of a file changed since the reference passed")
			}
			if _, err := os.Stat(filepath.Join(cwd, "f.txt")); err != nil {
				t.Fatalf("refused REM removed the file: %v", err)
			}
		})
	}
}
