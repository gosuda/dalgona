package read

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type statRunner struct{ runner.Runner }

func TestReadRequestedPathFailureStaysOrdinary(t *testing.T) {
	tests := []struct {
		name   string
		args   string
		remote bool
		locked bool
	}{
		{name: "missing file", args: `{"path":"missing.txt"}`},
		{name: "missing file through a remote runner", args: `{"path":"missing.txt"}`, remote: true},
		{name: "invalid arguments", args: `{"path":"f.txt","offset":0}`},
		{name: "unreadable file", args: `{"path":"locked.txt"}`, locked: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			env, cwd := readTestEnv(t)
			if test.locked {
				lockedFile(t, filepath.Join(cwd, "locked.txt"))
			}
			if test.remote {
				env.Runner = statRunner{Runner: env.Runner}
			}
			result, err := callRead(t, env, test.args)
			if err == nil {
				t.Fatal("the failing read returned no error")
			}
			if result.Failure != tool.FailureNone {
				t.Fatalf("failure = %q, want a requested-path failure to stay ordinary (error: %v)", result.Failure, err)
			}
		})
	}
}

func lockedFile(t *testing.T, path string) {
	t.Helper()
	if runtime.GOOS == "windows" || os.Getuid() == 0 {
		t.Skip("file permission bits do not refuse this user")
	}
	writeReadFixture(t, path, "secret\n")
	if err := os.Chmod(path, 0o000); err != nil {
		t.Fatal(err)
	}
}
