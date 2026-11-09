package remote

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/sandboxsvc/fake"
)

func TestRecordedBox_destroysBoxWhenRemoteRecordFails(t *testing.T) {
	home, err := os.Create(filepath.Join(t.TempDir(), "home-file"))
	if err != nil {
		t.Fatal(err)
	}
	if err := home.Close(); err != nil {
		t.Fatal(err)
	}
	b := NewBinder(Options{Config: config.Config{Home: home.Name()}})
	svc := fake.New()
	spec := config.RemoteSpec{Service: "docker"}
	if _, err := b.recordedBox(t.Context(), svc, "mote", spec); err == nil {
		t.Fatal("recordedBox error = nil, want settings write error")
	}
	if got := svc.DestroyCount(); got != 1 {
		t.Fatalf("destroyed %d boxes, want 1 orphan cleanup", got)
	}
}
