package sandbox0

import (
	"context"
	"errors"
	"io"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

func TestLivePauseResumePreservesWorkspace(t *testing.T) {
	if os.Getenv("SANDBOX0_TOKEN") == "" {
		t.Skip("SANDBOX0_TOKEN is required")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Minute)
	defer cancel()
	service, err := NewFromEnv(ctx, nil, nil)
	requireSandbox0(t, err)
	id, err := service.Create(ctx, sandboxsvc.Spec{Template: os.Getenv("SANDBOX0_TEMPLATE")})
	requireSandbox0(t, err)
	t.Cleanup(func() {
		cleanupCtx, cleanupCancel := context.WithTimeout(context.Background(), time.Minute)
		defer cleanupCancel()
		requireSandbox0(t, service.Destroy(cleanupCtx, id))
	})
	output, code, err := runSandbox0Command(ctx, service, id, []string{"sh", "-c", "printf sandbox0-persisted > /workspace/live.txt"})
	requireSandbox0(t, err)
	if code != 0 || output != "" {
		t.Fatalf("write command = code %d, output %q", code, output)
	}
	_, err = service.client.PauseSandboxAndWait(ctx, id, nil)
	requireSandbox0(t, err)
	current, err := service.Attach(ctx, id)
	requireSandbox0(t, err)
	if current != id {
		t.Fatalf("Attach id = %q, want %q", current, id)
	}
	output, code, err = runSandbox0Command(ctx, service, id, []string{"cat", "/workspace/live.txt"})
	requireSandbox0(t, err)
	if code != 0 || output != "sandbox0-persisted" {
		t.Fatalf("read command = code %d, output %q", code, output)
	}
	output, code, err = runSandbox0Command(ctx, service, id, []string{"uname", "-a"})
	requireSandbox0(t, err)
	if code != 0 || !strings.Contains(output, "Linux") {
		t.Fatalf("uname command = code %d, output %q", code, output)
	}
}

func runSandbox0Command(ctx context.Context, service *Service, id string, argv []string) (string, int, error) {
	stream, err := service.start(ctx, id, argv)
	if err != nil {
		return "", 0, err
	}
	output, readErr := io.ReadAll(stream)
	code, waitErr := stream.Wait()
	closeErr := stream.Close()
	return string(output), code, errors.Join(readErr, waitErr, closeErr)
}
