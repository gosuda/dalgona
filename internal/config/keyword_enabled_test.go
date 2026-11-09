package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestLoad_partialKeywordOverrideKeepsBuiltinEnabled(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDal, "[keywords.jevify]\neffort = \"high\"\n", "", config.Flags{})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Keywords["jevify"].Enabled {
		t.Fatal("partial jevify override on dal forced Enabled=true")
	}
}

func TestLoad_settingLayerDoesNotReenableDisabledKeyword(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "[keywords.ultrathink]\nenabled = false\n", "[keywords.ultrathink]\neffort = \"high\"\n", config.Flags{})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Keywords["ultrathink"].Enabled {
		t.Fatal("settings effort override re-enabled a keyword disabled in config.toml")
	}
}

func TestLoad_explicitEnabledWinsAcrossLayers(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "[keywords.ultrathink]\nenabled = false\n", "[keywords.ultrathink]\nenabled = true\n", config.Flags{})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if !cfg.Keywords["ultrathink"].Enabled {
		t.Fatal("explicit settings enabled=true was dropped")
	}
}
