package search

import (
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type policyProbe struct {
	root   string
	gen    string
	ref    indexRef
	builds *buildLog
	got    tool.Result
	want   tool.Result
}

func probeValidGrep(t *testing.T) policyProbe {
	return runPublicGrep(t, false)
}

func probeCorruptGrep(t *testing.T) policyProbe {
	return runPublicGrep(t, true)
}

func runPublicGrep(t *testing.T, corrupt bool) policyProbe {
	t.Helper()
	env, root := indexedEnv(t, map[string]string{"a.txt": "needle here\n", "b.txt": "other\n"})
	gen := currentGeneration(root)
	if gen == "" {
		t.Fatal("the fixture published no generation")
	}
	if corrupt {
		corruptGeneration(t, root, gen)
		setRebuildThreshold(t, 1)
	}
	ref := indexFor(env)
	builds := newBuildLog(t, nil)
	t.Cleanup(buildWG.Wait)
	raw := jsontext.Value(`{"mode":"grep","pattern":"needle","limit":100}`)
	parsed, err := parseArgs(raw)
	if err != nil {
		t.Fatal(err)
	}
	want, err := execute(env.Cwd, parsed, "", freshness{})
	if err != nil {
		t.Fatal(err)
	}
	got, err := New().Run(t.Context(), tool.Call{ID: "call-1", Args: raw}, env)
	if err != nil {
		t.Fatal(err)
	}
	return policyProbe{
		root: root, gen: gen, ref: ref, builds: builds,
		got: got, want: makeResult(want, snapshot.New()),
	}
}

func corruptGeneration(t *testing.T, root, gen string) {
	t.Helper()
	path := filepath.Join(root, gen, manifestFile)
	if err := os.WriteFile(path, []byte("not a manifest"), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestPublicGrepFollowsIndexPolicy(t *testing.T) {
	t.Run("valid", func(t *testing.T) { requireValidGrepPolicy(t) })
	t.Run("corrupt", func(t *testing.T) { requireCorruptGrepPolicy(t) })
}

func requireValidGrepPolicy(t *testing.T) {
	t.Helper()
	p := probeValidGrep(t)
	requirePolicyOutput(t, p)
	requireIndexFor(t, p)
	requireUntouchedGeneration(t, p)
	requirePolicySideEffects(t, p)
}

func requireCorruptGrepPolicy(t *testing.T) {
	t.Helper()
	p := probeCorruptGrep(t)
	requirePolicyOutput(t, p)
	requireIndexFor(t, p)
	requireCorruptConsequence(t, p)
	requirePolicySideEffects(t, p)
}

func requireCorruptConsequence(t *testing.T, p policyProbe) {
	t.Helper()
	if runtime.GOOS == "darwin" {
		requireUntouchedGeneration(t, p)
		return
	}
	requireQuarantine(t, p)
}

func requirePolicyOutput(t *testing.T, p policyProbe) {
	t.Helper()
	if normalizeRefs(p.got.Output) != normalizeRefs(p.want.Output) {
		t.Fatalf("public grep output %q, want %q", p.got.Output, p.want.Output)
	}
	if normalizeRefs(p.got.Meta) != normalizeRefs(p.want.Meta) {
		t.Fatalf("public grep meta %s, want %s", p.got.Meta, p.want.Meta)
	}
}

func requireIndexFor(t *testing.T, p policyProbe) {
	t.Helper()
	if runtime.GOOS == "darwin" {
		requireDarwinScan(t, p)
		return
	}
	requireIndexedGrep(t, p)
}

func requireDarwinScan(t *testing.T, p policyProbe) {
	t.Helper()
	if p.ref.root != "" || p.ref.gen != "" {
		t.Fatalf("indexFor on darwin = %+v, want no index", p.ref)
	}
}

func requireIndexedGrep(t *testing.T, p policyProbe) {
	t.Helper()
	if p.ref.root != p.root || p.ref.gen != p.gen {
		t.Fatalf("indexFor = %+v, want root %q and generation %q", p.ref, p.root, p.gen)
	}
}

func requireUntouchedGeneration(t *testing.T, p policyProbe) {
	t.Helper()
	if currentGeneration(p.root) != p.gen {
		t.Fatal("CURRENT no longer names the planted generation")
	}
	if _, err := os.Stat(filepath.Join(p.root, backoffFile)); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("the untouched index recorded a failure backoff: %v", err)
	}
}

func requireQuarantine(t *testing.T, p policyProbe) {
	t.Helper()
	if _, err := os.Stat(filepath.Join(p.root, currentFile)); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("CURRENT survived the quarantine: %v", err)
	}
	if gen := currentGeneration(p.root); gen != "" {
		t.Fatalf("CURRENT names %q after the quarantine, want none", gen)
	}
	if _, err := os.Stat(filepath.Join(p.root, backoffFile)); err != nil {
		t.Fatalf("the quarantine recorded no failure backoff: %v", err)
	}
}

func requirePolicySideEffects(t *testing.T, p policyProbe) {
	t.Helper()
	buildWG.Wait()
	if p.builds.count() != 0 {
		t.Fatalf("build starts = %d, want 0 above the threshold", p.builds.count())
	}
}
