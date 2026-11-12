package agent

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
)

func TestReproSnapshotPreimageMasked(t *testing.T) {
	const secret = "repro-secret-987654321"
	t.Setenv("REPRO_SNAP_API_KEY", secret)
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		store, err := auth.Load(h.home)
		if err != nil {
			t.Fatal(err)
		}
		h.cfg.Auth = store
	})
	h.write("secret.txt", "prefix "+secret+" suffix\nsecond line\n")
	events := runCalls(t, h,
		call{id: "r", name: "read", args: `{"path":"secret.txt"}`},
		call{id: "p", name: "patch", args: `{"changes":[{"path":"secret.txt","old":"second line","new":"replaced"}]}`},
	)
	if len(events) == 0 {
		t.Fatal("no events")
	}
	blobs, err := os.ReadDir(filepath.Join(h.home, "blobs"))
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range blobs {
		data, err := os.ReadFile(filepath.Join(h.home, "blobs", entry.Name()))
		if err != nil {
			t.Fatal(err)
		}
		if strings.Contains(string(data), secret) {
			t.Fatalf("blob %s contains raw secret", entry.Name())
		}
	}
}
