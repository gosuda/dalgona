package patch

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

func TestStagedBatchFreezesEvidenceAndBytes(t *testing.T) {
	env, cwd := editTestEnv(t)
	env.Evidence = snapshot.Evidence{Consumer: "model", Cutoff: 7}
	input := "[a.go#NEW]\nPUT >$:\n+package a"
	edits, err := parse(HashlineLight, input)
	if err != nil {
		t.Fatal(err)
	}
	batch, err := stage(t.Context(), HashlineLight, edits, env)
	if err != nil {
		t.Fatal(err)
	}
	defer batch.closePins()
	env.Evidence.Cutoff = 99
	edits[0].body = "changed"
	exposed := batch.changes()
	exposed[0].Post[0] = 'X'
	if batch.cutoff.Cutoff != 7 || batch.version != 1 || batch.style != HashlineLight {
		t.Fatalf("prepared identity changed: %+v", batch)
	}
	if _, err := applyBatch(t.Context(), env, batch); err != nil {
		t.Fatal(err)
	}
	if got := readFixture(t, cwd, "a.go"); got != "package a" {
		t.Fatalf("prepared bytes changed: %q", got)
	}
}

func TestObserverRetainsFindingsWithoutWriting(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "untouched", "sentinel")
	placeholder := "+// " + strings.Repeat(".", 3) + " rest of code"
	input := "[a.go#NEW]\nPUT >$:\n" + placeholder + "\n[b.go#NEW]\nPUT >$:\n" + placeholder
	edits, err := parse(HashlineLight, input)
	if err != nil {
		t.Fatal(err)
	}
	batch, err := stage(t.Context(), HashlineLight, edits, env)
	if err == nil || !strings.Contains(err.Error(), "omitted code") {
		t.Fatalf("observer error = %v", err)
	}
	if len(batch.findings) != 2 || batch.findings[0].path != "a.go" || batch.findings[1].path != "b.go" {
		t.Fatalf("findings = %+v", batch.findings)
	}
	assertMissingFiles(t, cwd, "a.go", "b.go")
	stageProseControl(t, env, cwd, input)
}

func assertMissingFiles(t *testing.T, cwd string, names ...string) {
	t.Helper()
	for _, name := range names {
		if _, err := os.Stat(filepath.Join(cwd, name)); !errors.Is(err, fs.ErrNotExist) {
			t.Fatalf("observer created %s: %v", name, err)
		}
	}
}

func stageProseControl(t *testing.T, env tool.Env, cwd, input string) {
	t.Helper()
	control := strings.ReplaceAll(input, ".go", ".txt")
	edits, err := parse(HashlineLight, control)
	if err != nil {
		t.Fatal(err)
	}
	accepted, err := stage(t.Context(), HashlineLight, edits, env)
	if err != nil {
		t.Fatal(err)
	}
	defer accepted.closePins()
	if len(accepted.findings) != 0 {
		t.Fatalf("prose control findings = %+v", accepted.findings)
	}
	if got := readFixture(t, cwd, "untouched"); got != "sentinel" {
		t.Fatalf("unrelated bytes changed: %q", got)
	}
}

func TestStagedBatchValidatesShapesBeforeEvidence(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.go", "package a\n")
	input := "[a.go@r0123456789abcdef0123456789abcdef.1]\nPUT 1:\n+package b\n[b.go#NEW]\nPUT >$:\n+one\n[b.go#NEW]\nPUT >$:\n+two"
	edits, err := parse(HashlineLight, input)
	if err != nil {
		t.Fatal(err)
	}
	_, err = stage(t.Context(), HashlineLight, edits, env)
	if err == nil || !strings.Contains(err.Error(), "cannot combine") {
		t.Fatalf("shape must fail before unavailable evidence: %v", err)
	}
	if got := readFixture(t, cwd, "a.go"); got != "package a\n" {
		t.Fatalf("valid prefix changed: %q", got)
	}
}

func TestStagedBatchReplaceOwnsApprovedBytes(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.go", "old\n")
	after := []byte("new\n")
	plan, err := PlanReplace(t.Context(), env, "rewrite", []ReplacedFile{{Path: "a.go", Before: []byte("old\n"), After: after}})
	if err != nil {
		t.Fatal(err)
	}
	defer plan.Discard()
	after[0] = 'X'
	plan.Files[0].Post[0] = 'Y'
	if _, err := plan.Apply(t.Context()); err != nil {
		t.Fatal(err)
	}
	if got := readFixture(t, cwd, "a.go"); got != "new\n" {
		t.Fatalf("caller mutated approved bytes: %q", got)
	}
}
