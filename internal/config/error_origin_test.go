package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestLoad_keywordErrorNamesSettingsOrigin(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "", "[keywords.bad]\nnotice = \"missing.md\"\n", config.Flags{})
	requireConfigError(t, err, "settings.toml: keyword \"bad\" notice file missing.md not found")
}

func TestLoad_editStyleErrorNamesSettingsOrigin(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "", "edit_style = \"magic\"\n", config.Flags{})
	requireConfigError(t, err, "settings.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict.")
}

func TestLoad_flagValueErrorNamesFlags(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{Approval: "bogus"})
	requireConfigError(t, err, "flags: approval must be one of auto, yolo, ask")
}
