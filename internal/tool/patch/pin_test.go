package patch

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func planFor(t *testing.T, env tool.Env, input string) tool.Plan {
	t.Helper()
	editor := New(string(Replace))
	plan, err := editor.Plan(t.Context(), tool.Call{ID: "call", Args: []byte(input)}, env)
	if err != nil {
		t.Fatalf("plan failed: %v", err)
	}
	return plan
}

func swapDirToSymlink(t *testing.T, dir, outside string) {
	t.Helper()
	moved := dir + "-moved"
	if err := os.Rename(dir, moved); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, dir); err != nil {
		t.Fatal(err)
	}
}

func requireFile(t *testing.T, path, want string) {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	if string(data) != want {
		t.Fatalf("%s = %q, want %q", path, data, want)
	}
}

func requireMissing(t *testing.T, path string) {
	t.Helper()
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("%s exists after the patch: %v", path, err)
	}
}

func TestPatchUpdateSurvivesParentSwapAfterPlan(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(outside, "f.txt"), []byte("before\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "sub/f.txt", "before\n")
	plan := planFor(t, env, `{"changes":[{"path":"sub/f.txt","old":"before","new":"after"}]}`)
	swapDirToSymlink(t, filepath.Join(cwd, "sub"), outside)
	if _, err := plan.Apply(t.Context()); err != nil {
		t.Fatalf("apply failed: %v", err)
	}
	requireFile(t, filepath.Join(outside, "f.txt"), "before\n")
	requireFile(t, filepath.Join(cwd, "sub-moved", "f.txt"), "after\n")
}

func TestPatchDeleteSurvivesParentSwapAfterPlan(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(outside, "f.txt"), []byte("before\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "sub/f.txt", "before\n")
	plan := planFor(t, env, `{"changes":[{"path":"sub/f.txt","delete":true}]}`)
	swapDirToSymlink(t, filepath.Join(cwd, "sub"), outside)
	if _, err := plan.Apply(t.Context()); err != nil {
		t.Fatalf("apply failed: %v", err)
	}
	requireFile(t, filepath.Join(outside, "f.txt"), "before\n")
	requireMissing(t, filepath.Join(cwd, "sub-moved", "f.txt"))
}

func TestPatchRenameSurvivesParentSwapAfterPlan(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(outside, "f.txt"), []byte("before\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "sub/f.txt", "before\n")
	plan := planFor(t, env, `{"changes":[{"path":"sub/f.txt","old":"before","new":"after"},{"path":"sub/f.txt","rename":"sub/g.txt"}]}`)
	swapDirToSymlink(t, filepath.Join(cwd, "sub"), outside)
	if _, err := plan.Apply(t.Context()); err != nil {
		t.Fatalf("apply failed: %v", err)
	}
	requireFile(t, filepath.Join(outside, "f.txt"), "before\n")
	requireMissing(t, filepath.Join(outside, "g.txt"))
	requireMissing(t, filepath.Join(cwd, "sub-moved", "f.txt"))
	requireFile(t, filepath.Join(cwd, "sub-moved", "g.txt"), "after\n")
}

func TestPatchCreateRejectsSymlinkedMissingParent(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatal(err)
	}
	plan := planFor(t, env, `{"changes":[{"path":"nd/g.txt","create":"content\n"}]}`)
	if err := os.Symlink(outside, filepath.Join(cwd, "nd")); err != nil {
		t.Fatal(err)
	}
	if _, err := plan.Apply(t.Context()); err == nil {
		t.Fatal("create into a symlinked missing parent succeeded")
	}
	requireMissing(t, filepath.Join(outside, "g.txt"))
}

func TestPatchCreateVerifySeesLeafThroughGapCreatedParent(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	plan := planFor(t, env, `{"changes":[{"path":"nd/g.txt","create":"content\n"}]}`)
	if err := os.MkdirAll(filepath.Join(cwd, "nd"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(cwd, "nd", "g.txt"), []byte("appeared\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := plan.Apply(t.Context()); err == nil {
		t.Fatal("create over a file that appeared during the gap succeeded")
	}
	requireFile(t, filepath.Join(cwd, "nd", "g.txt"), "appeared\n")
}

func TestPatchCreateIntoMissingParentAllowsUnrelatedRootLeaf(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "g.txt", "unrelated\n")
	plan := planFor(t, env, `{"changes":[{"path":"nd/g.txt","create":"content\n"}]}`)
	if _, err := plan.Apply(t.Context()); err != nil {
		t.Fatalf("create blocked by an unrelated root leaf: %v", err)
	}
	requireFile(t, filepath.Join(cwd, "g.txt"), "unrelated\n")
	requireFile(t, filepath.Join(cwd, "nd", "g.txt"), "content\n")
}
