package config_test

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestWriteSetting_concurrentWritesKeepBothKeys(t *testing.T) {
	for round := range 100 {
		home := t.TempDir()
		var done sync.WaitGroup
		done.Add(2)
		for _, item := range []struct{ key, value string }{{"model", "a"}, {"runner", "b"}} {
			go func(key, value string) {
				defer done.Done()
				if err := config.WriteSettings(home, config.SettingEdit{Key: key, Value: value}); err != nil {
					t.Errorf("WriteSettings(%s): %v", key, err)
				}
			}(item.key, item.value)
		}
		done.Wait()
		raw, err := os.ReadFile(filepath.Join(home, "settings.toml"))
		if err != nil {
			t.Fatalf("round %d: %v", round, err)
		}
		if !settingsKeySet(string(raw), "model", "a") || !settingsKeySet(string(raw), "runner", "b") {
			t.Fatalf("round %d: concurrent writes lost a key: %s", round, raw)
		}
	}
}

func settingsKeySet(raw, key, value string) bool {
	return strings.Contains(raw, key+" = \""+value+"\"") || strings.Contains(raw, key+" = '"+value+"'")
}
