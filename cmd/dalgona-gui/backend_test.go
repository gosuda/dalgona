//go:build gui

package main

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func TestPackagedSiblingBackend(t *testing.T) {
	t.Setenv(backendVariable, "")
	dir := t.TempDir()
	exe, daemon := "dalgona-gui", "dalgona"
	if runtime.GOOS == "windows" {
		exe, daemon = "dalgona-gui.exe", "dalgona.exe"
	}
	want := filepath.Join(dir, daemon)
	if err := os.WriteFile(want, []byte("installed daemon"), 0o700); err != nil {
		t.Fatal(err)
	}
	backend, err := client.ResolveBackend(backendPath(filepath.Join(dir, exe)))
	if err != nil {
		t.Fatal(err)
	}
	if filepath.Base(backend.Path) != daemon || backend.Build == "" {
		t.Fatalf("resolved backend = %+v; want %s with a build identity", backend, want)
	}
}
