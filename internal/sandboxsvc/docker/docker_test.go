package docker_test

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"os"
	"strconv"
	"testing"
	"time"

	cerrdefs "github.com/containerd/errdefs"
	"github.com/moby/moby/client"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
	"github.com/gosuda/dalgona/internal/sandboxsvc/docker"
)

type dockerCommand struct {
	argv   []string
	output string
	code   int
}

func TestServiceLifecycle(t *testing.T) {
	cli := newDockerClient(t)
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Minute)
	defer cancel()
	service, id, run := newAttachedDockerBox(t, ctx, cli, skipBootstrap)
	assertDockerCommand(t, ctx, run, dockerCommand{
		argv:   []string{"echo", "hi"},
		output: "hi\n",
	})
	assertDockerCommand(t, ctx, run, dockerCommand{
		argv: []string{"sh", "-c", "exit 3"},
		code: 3,
	})
	destroyDockerBox(t, ctx, service, cli, id)
}

func TestBootstrapUploadsBinaryIntact(t *testing.T) {
	cli := newDockerClient(t)
	ctx, cancel := context.WithTimeout(t.Context(), 3*time.Minute)
	defer cancel()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	binary, err := os.ReadFile(exe)
	if err != nil {
		t.Fatal(err)
	}
	if len(binary) < 3<<20 {
		t.Fatalf("test binary is %d bytes; the upload check needs at least 3 MiB", len(binary))
	}
	sum := sha256.Sum256(binary)
	service, id, run := newAttachedDockerBox(t, ctx, cli, runner.Bootstrap)
	assertDockerCommand(t, ctx, run, dockerCommand{
		argv:   []string{"sh", "-c", `sha256sum "$HOME/.dalgona/bin/dalgona" | cut -d " " -f 1`},
		output: hex.EncodeToString(sum[:]) + "\n",
	})
	destroyDockerBox(t, ctx, service, cli, id)
}

func skipBootstrap(context.Context, string, runner.RunFunc) error { return nil }

func newDockerClient(t *testing.T) *client.Client {
	t.Helper()
	cli, err := client.New(client.FromEnv)
	if err != nil {
		t.Fatalf("create Docker client: %v", err)
	}
	pingCtx, cancelPing := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancelPing()
	ping, pingErr := cli.Ping(pingCtx, client.PingOptions{NegotiateAPIVersion: true})
	if pingErr != nil {
		t.Skipf("Docker daemon did not answer: %v", pingErr)
	}
	if ping.OSType != "" && ping.OSType != "linux" {
		t.Skipf("Docker daemon runs %s containers", ping.OSType)
	}
	return cli
}

func newAttachedDockerBox(t *testing.T, ctx context.Context, cli *client.Client, bootstrap runner.BootstrapFunc) (*docker.Service, string, runner.RunFunc) {
	t.Helper()
	var run runner.RunFunc
	service := docker.New(cli, func(ctx context.Context, id string, runFunc runner.RunFunc) error {
		run = runFunc
		return bootstrap(ctx, id, runFunc)
	})
	name := "dalgona-docker-integration-" + strconv.FormatInt(time.Now().UnixNano(), 10)
	id, createErr := service.Create(ctx, sandboxsvc.Spec{
		Name:  name,
		Image: "docker.io/library/buildpack-deps:stable",
	})
	if createErr != nil {
		t.Fatalf("create box: %v", createErr)
	}
	t.Cleanup(func() {
		_, _ = cli.ContainerRemove(context.Background(), id, client.ContainerRemoveOptions{Force: true})
	})
	attached, attachErr := service.Attach(ctx, id)
	if attachErr != nil {
		t.Fatalf("attach box: %v", attachErr)
	}
	if attached != id {
		t.Fatalf("attached id %q, want %q", attached, id)
	}
	if run == nil {
		t.Fatal("attach did not supply a run function")
	}
	return service, id, run
}

func assertDockerCommand(t *testing.T, ctx context.Context, run runner.RunFunc, command dockerCommand) {
	t.Helper()
	stream, runErr := run(ctx, command.argv)
	if runErr != nil {
		t.Fatalf("run %v: %v", command.argv, runErr)
	}
	output, readErr := io.ReadAll(stream)
	if readErr != nil {
		t.Fatalf("read %v output: %v", command.argv, readErr)
	}
	exitCode, waitErr := stream.Wait()
	if waitErr != nil {
		t.Fatalf("wait for %v: %v", command.argv, waitErr)
	}
	if exitCode != command.code {
		t.Fatalf("%v exit code %d, want %d", command.argv, exitCode, command.code)
	}
	if string(output) != command.output {
		t.Fatalf("%v output %q, want %q", command.argv, output, command.output)
	}
	if closeErr := stream.Close(); closeErr != nil {
		t.Fatalf("close %v stream: %v", command.argv, closeErr)
	}
}

func destroyDockerBox(t *testing.T, ctx context.Context, service *docker.Service, cli *client.Client, id string) {
	t.Helper()
	if destroyErr := service.Destroy(ctx, id); destroyErr != nil {
		t.Fatalf("destroy box: %v", destroyErr)
	}
	if _, inspectErr := cli.ContainerInspect(ctx, id, client.ContainerInspectOptions{}); !cerrdefs.IsNotFound(inspectErr) {
		t.Fatalf("inspect destroyed box: err = %v, want not found", inspectErr)
	}
}
