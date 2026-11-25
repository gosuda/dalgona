package rewrite

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

func TestRewriteOrdinaryFailureStaysOrdinary(t *testing.T) {
	t.Run("invalid arguments", func(t *testing.T) {
		env := setup(t, map[string]string{"a.go": "package a\n"})
		result, err := run(t, env, args{Lang: "cobol", Pattern: funcName, Target: "name", Replace: "x"})
		if err == nil {
			t.Fatal("an unknown lang was accepted")
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("failure = %q, want invalid arguments to stay ordinary", result.Failure)
		}
	})
	t.Run("read-only directory", func(t *testing.T) {
		if runtime.GOOS == "windows" || os.Getuid() == 0 {
			t.Skip("directory permission bits do not refuse this user")
		}
		env := setup(t, map[string]string{"sub/a.go": "package a\n\nfunc Keep() {}\n"})
		dir := filepath.Join(env.Cwd, "sub")
		if err := os.Chmod(dir, 0o555); err != nil { //nolint:gosec // read-only fixture stays traversable while refusing writes
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = os.Chmod(dir, 0o700) }) //nolint:gosec // restore owner traversal so TempDir cleanup removes the fixture
		result, err := run(t, env, goCall(funcName, "name", "Renamed"))
		if err == nil {
			t.Fatal("a rewrite into a read-only directory succeeded")
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("failure = %q, want a refused requested path to stay ordinary (error: %v)", result.Failure, err)
		}
	})
}
