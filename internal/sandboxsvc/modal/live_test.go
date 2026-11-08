package modal

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

func TestLiveVolumeSurvivesSandboxReplacement(t *testing.T) {
	if os.Getenv("MODAL_TOKEN_ID") == "" || os.Getenv("MODAL_TOKEN_SECRET") == "" {
		t.Skip("MODAL_TOKEN_ID and MODAL_TOKEN_SECRET are required")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 15*time.Minute)
	defer cancel()
	service, err := NewFromEnv(nil)
	requireModal(t, err)
	name := fmt.Sprintf("dalgona-live-%d", time.Now().UnixNano())
	id, err := service.Create(ctx, sandboxsvc.Spec{Name: name})
	requireModal(t, err)
	current := id
	t.Cleanup(func() {
		cleanupCtx, cleanupCancel := context.WithTimeout(context.Background(), time.Minute)
		defer cleanupCancel()
		requireModal(t, service.Destroy(cleanupCtx, current))
	})
	output, code, err := runModalCommand(ctx, service, id, []string{"sh", "-c", `test "$HOME" = /data/home && test "$DALGONA_SYNCFS" = /data && printf modal-persisted > /workspace/live.txt && sync /data`})
	requireModal(t, err)
	if code != 0 || output != "" {
		t.Fatalf("write command = code %d, output %q", code, output)
	}
	sandboxID, _, err := splitID(id)
	requireModal(t, err)
	box, err := service.api.FromID(ctx, sandboxID)
	requireModal(t, err)
	requireModal(t, box.Terminate(ctx))
	current, err = service.Attach(ctx, id)
	requireModal(t, err)
	if current == id || !strings.HasSuffix(current, "/"+name) {
		t.Fatalf("replacement id = %q, original %q", current, id)
	}
	output, code, err = runModalCommand(ctx, service, current, []string{"cat", "/workspace/live.txt"})
	requireModal(t, err)
	if code != 0 || output != "modal-persisted" {
		t.Fatalf("read command = code %d, output %q", code, output)
	}
}

func runModalCommand(ctx context.Context, service *Service, id string, argv []string) (string, int, error) {
	sandboxID, _, err := splitID(id)
	if err != nil {
		return "", 0, err
	}
	box, err := service.api.FromID(ctx, sandboxID)
	if err != nil {
		return "", 0, fmt.Errorf("get box: %w", err)
	}
	stream, err := service.start(ctx, box, argv)
	if err != nil {
		return "", 0, err
	}
	output, readErr := io.ReadAll(stream)
	code, waitErr := stream.Wait()
	closeErr := stream.Close()
	return string(output), code, errors.Join(readErr, waitErr, closeErr)
}
