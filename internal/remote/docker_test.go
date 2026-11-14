package remote

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/moby/moby/client"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/runner"
)

func TestDockerBindRecoverAndPull(t *testing.T) {
	requireDocker(t)
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Minute)
	defer cancel()
	store := openJournal(t)
	cwd := gitWorkspace(t)
	thread := "docker-" + strconv.FormatInt(time.Now().UnixNano(), 36)
	startThread(t, store, thread, cwd)
	cfg := config.Config{Home: t.TempDir(), Runners: map[string]config.RunnerSpec{
		"box": {Service: "docker", Image: "docker.io/library/buildpack-deps:stable"},
	}}
	t.Cleanup(func() { removeContainer("dalgona-" + thread) })

	first := openRunner(ctx, t, dockerBinder(cfg, store), thread)
	spec := runner.ProcessSpec{ID: "call-1", Argv: []string{"sh", "-c", "sleep 2; go version || echo nogo"}, Dir: cwd}
	must(t, first.Start(ctx, spec))
	must(t, first.Close())

	restarted := dockerBinder(cfg, store)
	second := openRunner(ctx, t, restarted, thread)
	defer func() { _ = second.Close() }()
	if out := waitOutput(ctx, t, second, "call-1"); !strings.Contains(out, "nogo") && !strings.Contains(out, "go version") {
		t.Fatalf("exec output %q", out)
	}
	if got := bindings(t, store, thread); len(got) != 1 {
		t.Fatalf("bindings after restart = %+v", got)
	}

	must(t, second.WriteFile(ctx, filepath.Join(cwd, "hello.txt"), []byte("two\n"), 0o644))
	must(t, second.WriteFile(ctx, filepath.Join(cwd, "new.txt"), []byte("new\n"), 0o644))
	must(t, restarted.Pull(ctx, thread))
	requireFiles(t, cwd, map[string]string{"hello.txt": "two\n", "new.txt": "new\n"})
	if _, err := os.Stat(filepath.Join(cwd, ".dalgona-baseline")); !os.IsNotExist(err) {
		t.Fatalf("pull copied the baseline marker: %v", err)
	}
	must(t, restarted.Archive(ctx, thread))
}

func requireFiles(t *testing.T, dir string, want map[string]string) {
	t.Helper()
	for name, content := range want {
		got, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil || string(got) != content {
			t.Fatalf("local %s = %q, %v; want %q", name, got, err, content)
		}
	}
}

func dockerBinder(cfg config.Config, store *journal.Store) *Binder {
	b := NewBinder(Options{Config: cfg, Journal: store})
	b.SetRecorder(&journalRecorder{store: store})
	return b
}

func requireDocker(t *testing.T) {
	t.Helper()
	cli, err := client.New(client.FromEnv)
	if err != nil {
		t.Skipf("Docker client: %v", err)
	}
	defer func() { _ = cli.Close() }()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	ping, pingErr := cli.Ping(ctx, client.PingOptions{NegotiateAPIVersion: true})
	if pingErr != nil {
		t.Skipf("Docker daemon did not answer: %v", pingErr)
	}
	if ping.OSType != "" && ping.OSType != "linux" {
		t.Skipf("Docker daemon runs %s containers", ping.OSType)
	}
}

func removeContainer(name string) {
	cli, err := client.New(client.FromEnv)
	if err != nil {
		return
	}
	defer func() { _ = cli.Close() }()
	_, _ = cli.ContainerRemove(context.Background(), name, client.ContainerRemoveOptions{Force: true})
}

func gitWorkspace(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "hello.txt"), []byte("one\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	for _, args := range [][]string{
		{"init", "-q"},
		{"add", "-A"},
		{"-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-q", "-m", "init"},
	} {
		cmd := exec.Command("git", args...)
		cmd.Dir = dir
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %v: %v: %s", args, err, out)
		}
	}
	return dir
}
