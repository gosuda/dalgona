package agent

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestHostWriteSettingsIsLive(t *testing.T) {
	h := newHarness(t)
	if err := h.host.WriteSettings(config.SettingEdit{Key: "reasoning", Value: "high"}); err != nil {
		t.Fatal(err)
	}
	if got := h.host.Config().Reasoning; got != config.ReasoningHigh {
		t.Fatalf("reasoning = %q, want high", got)
	}

	settings := filepath.Join(h.home, "settings.toml")
	before, err := os.ReadFile(settings)
	if err != nil {
		t.Fatal(err)
	}
	verbosity := h.host.Config().Verbosity
	if err = h.host.WriteSettings(config.SettingEdit{Key: "verbosity", Value: "loud"}); err == nil {
		t.Fatal("WriteSettings accepted an invalid verbosity")
	}
	if got := h.host.Config().Verbosity; got != verbosity {
		t.Fatalf("verbosity = %q after a rejected edit, want %q", got, verbosity)
	}
	after, err := os.ReadFile(settings)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(before, after) {
		t.Fatalf("settings.toml changed after a rejected edit: %q", after)
	}
}
