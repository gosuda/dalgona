package config_test

import (
	"bytes"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func requireLoadFailure(t *testing.T, settingsBody, want string) {
	t.Helper()
	_, err := loadConfig(t, config.ProductDalgona, "", settingsBody, config.Flags{})
	if err == nil || !strings.Contains(err.Error(), want) {
		t.Fatalf("error = %v, want it to contain %q", err, want)
	}
}

func TestInteractiveSettingsDefaultsAndValidation(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Verbosity, config.VerbosityMedium)
	requireConfigValue(t, cfg.Theme, config.ThemeAuto)
	requireConfigValue(t, cfg.Icons, config.IconsAuto)
	if len(cfg.StatusHide) != 0 {
		t.Fatalf("status_hide = %v, want empty", cfg.StatusHide)
	}
	requireConfigValue(t, cfg.Notifications, true)

	cfg, err = loadConfig(t, config.ProductDalgona, "", "theme = \"auto-soft\"\nicons = \"nerd\"\n", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Theme, config.ThemeAutoSoft)
	requireConfigValue(t, cfg.Icons, config.IconsNerd)

	requireLoadFailure(t, "icons = \"emoji\"\n", "icons must be one of auto, unicode, nerd")
	requireLoadFailure(t, "verbosity = \"loud\"\n", "verbosity must be one of low, medium, high")
	requireLoadFailure(t, "status_hide = [\"model\"]\n", "status_hide: model cannot be hidden")
	requireLoadFailure(t, "status_hide = [\"nope\"]\n", "status_hide: unsupported item nope")
}

func TestConfigWithAppliesAndRejectsSettings(t *testing.T) {
	cfg, err := (config.Config{Product: config.ProductDalgona}).With(
		config.SettingEdit{Key: "verbosity", Value: "low"},
		config.SettingEdit{Key: "status_hide", Value: []string{"folder", "queue"}},
		config.SettingEdit{Key: "edit_review.enabled", Value: true},
	)
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Verbosity, config.VerbosityLow)
	if !slices.Equal(cfg.StatusHide, []string{"folder", "queue"}) {
		t.Fatalf("status_hide = %v", cfg.StatusHide)
	}
	requireConfigValue(t, cfg.EditReview.Enabled, true)
	if got := cfg.Setting("verbosity"); got != "low" {
		t.Fatalf("Setting(verbosity) = %v, want low", got)
	}

	for _, test := range []struct {
		edit config.SettingEdit
		want string
	}{
		{edit: config.SettingEdit{Key: "approval", Value: "edits"}, want: "approval must be one of auto, yolo, ask"},
		{edit: config.SettingEdit{Key: "notifications", Value: "yes"}, want: "notifications must be a boolean"},
		{edit: config.SettingEdit{Key: "status_hide", Value: "folder"}, want: "status_hide must be a list of strings"},
		{edit: config.SettingEdit{Key: "status_hide", Value: []string{"model"}}, want: "status_hide: model cannot be hidden"},
		{edit: config.SettingEdit{Key: "remotes.work", Value: "x"}, want: "unsupported setting remotes.work"},
	} {
		_, err := config.Config{}.With(test.edit)
		if err == nil || err.Error() != test.want {
			t.Fatalf("With(%s) error = %v, want %q", test.edit.Key, err, test.want)
		}
	}
}

func TestConfigWithRejectsDalgonaRootsForDal(t *testing.T) {
	for _, root := range []string{"runner", "remote", "runners", "remotes", "mcp_servers", "mcp_trusted_workspaces", "discovery", "herdr", "web", "edit_review"} {
		t.Run(root, func(t *testing.T) {
			for _, edit := range []config.SettingEdit{
				{Key: root, Value: true},
				{Key: root, Value: false},
				{Key: root + ".enabled", Value: true},
				{Key: root + ".enabled", Value: false},
			} {
				cfg := config.Config{Product: config.ProductDal, Theme: config.ThemeAuto}
				_, err := cfg.With(
					config.SettingEdit{Key: "theme", Value: "night"},
					edit,
				)
				if err == nil || err.Error() != root+" is a dalgona setting" {
					t.Fatalf("With(%s, %v) error = %v", edit.Key, edit.Value, err)
				}
				if cfg.Theme != config.ThemeAuto {
					t.Fatalf("rejected batch changed the original config: %+v", cfg)
				}
			}
		})
	}
}

func TestWriteSettingsIsAllOrNothing(t *testing.T) {
	home := t.TempDir()
	original := "model = \"openai/gpt-6\"\n"
	writeConfigFile(t, home, "settings.toml", original)
	err := config.WriteSettings(home,
		config.SettingEdit{Key: "model", Value: "openai/gpt-7"},
		config.SettingEdit{Key: "verbosity", Value: "loud"},
	)
	if err == nil {
		t.Fatal("WriteSettings accepted an invalid verbosity")
	}
	raw, readErr := os.ReadFile(filepath.Join(home, "settings.toml"))
	if readErr != nil {
		t.Fatal(readErr)
	}
	if !bytes.Equal(raw, []byte(original)) {
		t.Fatalf("settings.toml changed: %q", raw)
	}
}
