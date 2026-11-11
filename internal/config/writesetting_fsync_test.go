package config_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestWriteSetting_durableWrite(t *testing.T) {
	home := t.TempDir()
	if err := config.WriteSetting(home, "model", "m"); err != nil {
		t.Fatalf("write: %v", err)
	}
	path := filepath.Join(home, "settings.toml")
	assertOwnerOnly(t, path)
	raw, err := os.ReadFile(path)
	if err != nil || !settingsKeySet(string(raw), "model", "m") {
		t.Fatalf("content = %q, err = %v", raw, err)
	}
	entries, err := os.ReadDir(home)
	if err != nil {
		t.Fatalf("readdir: %v", err)
	}
	for _, entry := range entries {
		if strings.HasPrefix(entry.Name(), ".settings-") {
			t.Fatalf("leftover temp file %s", entry.Name())
		}
	}
}
