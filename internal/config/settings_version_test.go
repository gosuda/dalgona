package config_test

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
)

func TestWriteSettingsIfRejectsWriteThatLostTheRace(t *testing.T) {
	home := t.TempDir()
	path := filepath.Join(home, "settings.toml")
	original := "model = \"openai/gpt-6\"\n"
	writeConfigFile(t, home, "settings.toml", original)
	sum := sha256.Sum256([]byte(original))
	expected := hex.EncodeToString(sum[:])
	newer := "model = \"openai/gpt-8\"\n"

	unlock, err := auth.LockHome(home)
	if err != nil {
		t.Fatal(err)
	}
	result := make(chan error, 1)
	go func() {
		_, writeErr := config.WriteSettingsIf(home, expected, config.SettingEdit{Key: "model", Value: "openai/gpt-7"})
		result <- writeErr
	}()
	if writeErr := os.WriteFile(path, []byte(newer), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	if unlockErr := unlock(); unlockErr != nil {
		t.Fatal(unlockErr)
	}
	err = <-result
	if !errors.Is(err, config.ErrSettingsChanged) {
		t.Fatalf("WriteSettingsIf error = %v, want ErrSettingsChanged", err)
	}
	if want := "settings.toml changed; expected version " + expected; err.Error() != want {
		t.Fatalf("error = %q, want %q", err.Error(), want)
	}
	raw, readErr := os.ReadFile(path)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if string(raw) != newer {
		t.Fatalf("settings.toml = %q, want the newer write %q", raw, newer)
	}
}

func TestWriteSettingsIfReturnsVersionOfWrittenBytes(t *testing.T) {
	home := t.TempDir()
	path := filepath.Join(home, "settings.toml")
	version, err := config.WriteSettingsIf(home, "", config.SettingEdit{Key: "model", Value: "openai/gpt-7"})
	if err != nil {
		t.Fatal(err)
	}
	raw, readErr := os.ReadFile(path)
	if readErr != nil {
		t.Fatal(readErr)
	}
	sum := sha256.Sum256(raw)
	if version != hex.EncodeToString(sum[:]) {
		t.Fatalf("version = %s, want the SHA-256 of the written bytes", version)
	}
}
