package rpc

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	json "encoding/json/v2"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

type settingRead struct {
	Key          string `json:"key"`
	Value        any    `json:"value"`
	DefaultValue any    `json:"defaultValue"`
	Options      []any  `json:"options"`
}

type settingsReadResult struct {
	Settings []settingRead `json:"settings"`
}

type batchWriteResult struct {
	FilePath string `json:"filePath"`
	Status   string `json:"status"`
	Version  string `json:"version"`
}

func setDefaultSettings(t *testing.T, r *rig) {
	t.Helper()
	r.cfg.Config.Verbosity = config.VerbosityMedium
	r.cfg.Config.Theme = config.ThemeAuto
	r.cfg.Config.Icons = config.IconsAuto
	r.cfg.Config.Notifications = true
	reopen(t, r)
}

func readSettings(t *testing.T, c *wireClient) map[string]any {
	t.Helper()
	var result settingsReadResult
	c.ok("dalgona/settings/read", map[string]any{}, &result)
	values := map[string]any{}
	for _, item := range result.Settings {
		values[item.Key] = item.Value
	}
	return values
}

func expectVerbosityAndTheme(t *testing.T, c *wireClient, verbosity, theme string) {
	t.Helper()
	values := readSettings(t, c)
	if values["verbosity"] != verbosity || values["theme"] != theme {
		t.Fatalf("settings = verbosity %v theme %v, want %s and %s", values["verbosity"], values["theme"], verbosity, theme)
	}
}

func batchEdit(keyPath string, value any) map[string]any {
	return map[string]any{"keyPath": keyPath, "value": value, "mergeStrategy": "replace"}
}

func fileVersion(t *testing.T, path string) string {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(raw)
	return hex.EncodeToString(sum[:])
}

func TestSettingWriteReachesTheWorker(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	requests := recordRequestsListing(t, r, `{"data":[{"id":"gpt-6.1-sol"}]}`)
	r.cfg.Config.Model = "openai/gpt-6.1-sol"
	reopen(t, r)
	c := r.connect()
	c.ok("config/value/write", batchEdit("model_reasoning_effort", "high"), nil)
	threadID := startThread(t, r, c, nil)
	overrideTurn(t, c, prompt(threadID, "hello"))
	seen := requests.seen()
	if len(seen) == 0 {
		t.Fatal("no model request reached the fake provider")
	}
	if got := seen[len(seen)-1].effort; got != "high" {
		t.Fatalf("model request effort = %q, want high", got)
	}
}

func TestBatchWriteIsAllOrNothing(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	setDefaultSettings(t, r)
	c := r.connect()
	path := filepath.Join(r.home, "settings.toml")
	c.fails("config/batchWrite", map[string]any{"edits": []any{
		batchEdit("verbosity", "low"),
		batchEdit("theme", "dusk"),
	}}, codeInvalidParams, "unsupported theme dusk")
	if _, err := os.Stat(path); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("a failed batch wrote %s: %v", path, err)
	}
	expectVerbosityAndTheme(t, c, "medium", "auto")
	var written batchWriteResult
	c.ok("config/batchWrite", map[string]any{"edits": []any{
		batchEdit("verbosity", "low"),
		batchEdit("theme", "night"),
	}}, &written)
	if written.Status != "ok" || written.FilePath != path || written.Version != fileVersion(t, path) {
		t.Fatalf("batch result = %+v, file sha %s", written, fileVersion(t, path))
	}
	expectVerbosityAndTheme(t, c, "low", "night")
	c.fails("config/batchWrite", map[string]any{
		"edits":           []any{batchEdit("verbosity", "high")},
		"expectedVersion": "stale",
	}, codeInvalidRequest, "settings.toml changed; expected version stale")
	expectVerbosityAndTheme(t, c, "low", "night")
}

func TestBatchWriteRefusesEmptyEdits(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	path := filepath.Join(r.home, "settings.toml")
	c.fails("config/batchWrite", map[string]any{"edits": []any{}}, codeInvalidParams, "edits must not be empty")
	c.fails("config/batchWrite", map[string]any{}, codeInvalidParams, "edits must not be empty")
	if _, err := os.Stat(path); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("an empty batch wrote %s: %v", path, err)
	}
}

func TestSettingsReadListsInteractiveSettings(t *testing.T) {
	r := newRig(t, config.ApprovalAsk)
	r.cfg.Config.Product = config.ProductDalgona
	setDefaultSettings(t, r)
	c := r.connect()
	var result settingsReadResult
	c.ok("dalgona/settings/read", map[string]any{}, &result)
	want := []settingRead{
		{Key: "approval", Value: "ask", DefaultValue: "auto", Options: []any{"auto", "yolo", "ask"}},
		{Key: "verbosity", Value: "medium", DefaultValue: "medium", Options: []any{"low", "medium", "high"}},
		{Key: "theme", Value: "auto", DefaultValue: "auto", Options: []any{"auto", "auto-soft", "night", "soft-night", "day", "soft-day"}},
		{Key: "icons", Value: "auto", DefaultValue: "auto", Options: []any{"auto", "unicode", "nerd"}},
		{Key: "statusHide", Value: []any{}, DefaultValue: []any{}, Options: []any{"effort", "approval", "sandbox", "folder", "queue", "context", "mark"}},
		{Key: "editReview", Value: false, DefaultValue: false, Options: []any{true, false}},
		{Key: "notifications", Value: true, DefaultValue: true, Options: []any{true, false}},
	}
	if len(result.Settings) != len(want) {
		t.Fatalf("settings = %d entries, want %d", len(result.Settings), len(want))
	}
	for i := range want {
		if !reflect.DeepEqual(result.Settings[i], want[i]) {
			t.Fatalf("setting %d = %+v, want %+v", i, result.Settings[i], want[i])
		}
	}
	raw, err := json.Marshal(result)
	if err != nil {
		t.Fatal(err)
	}
	validateSchema(t, "dalgona-schema", "dalgona.settings.read.result.json", raw)
}

func TestDalSettingWritesRejectEditReviewBeforePersistence(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Product = config.ProductDal
	setDefaultSettings(t, r)
	c := r.connect()
	path := filepath.Join(r.home, "settings.toml")
	c.ok("config/value/write", batchEdit("theme", "night"), nil)
	before, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, enabled := range []bool{true, false} {
		c.fails("config/value/write", batchEdit("editReview", enabled), codeInvalidParams, "edit_review is a dalgona setting")
		c.fails("config/batchWrite", map[string]any{"edits": []any{
			batchEdit("editReview", enabled),
		}}, codeInvalidParams, "edit_review is a dalgona setting")
		c.fails("config/batchWrite", map[string]any{"edits": []any{
			batchEdit("theme", "day"),
			batchEdit("editReview", enabled),
		}}, codeInvalidParams, "edit_review is a dalgona setting")
		after, readErr := os.ReadFile(path)
		if readErr != nil {
			t.Fatal(readErr)
		}
		if !bytes.Equal(before, after) {
			t.Fatalf("rejected edit changed settings.toml: %q", after)
		}
		expectVerbosityAndTheme(t, c, "medium", "night")
		if r.host.Config().EditReview.Enabled {
			t.Fatal("rejected edit enabled live edit review")
		}
		cfg, loadErr := config.Load(config.ProductDal, r.cwd, config.Flags{})
		if loadErr != nil {
			t.Fatalf("reload dal settings: %v", loadErr)
		}
		if cfg.Theme != config.ThemeNight || cfg.EditReview.Enabled {
			t.Fatalf("reloaded dal settings = %+v", cfg)
		}
	}
}

func TestDalgonaSettingWritesAcceptEditReviewToggle(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Product = config.ProductDalgona
	t.Setenv("DALGONA_HOME", r.home)
	setDefaultSettings(t, r)
	c := r.connect()
	for _, enabled := range []bool{true, false} {
		c.ok("config/value/write", batchEdit("editReview", enabled), nil)
		if got := readSettings(t, c)["editReview"]; got != enabled {
			t.Fatalf("live editReview = %v, want %t", got, enabled)
		}
		cfg, err := config.Load(config.ProductDalgona, r.cwd, config.Flags{})
		if err != nil {
			t.Fatal(err)
		}
		if cfg.EditReview.Enabled != enabled {
			t.Fatalf("persisted edit review = %t, want %t", cfg.EditReview.Enabled, enabled)
		}
		c.ok("config/batchWrite", map[string]any{"edits": []any{
			batchEdit("editReview", !enabled),
		}}, nil)
		if got := readSettings(t, c)["editReview"]; got != !enabled {
			t.Fatalf("batch editReview = %v, want %t", got, !enabled)
		}
	}
}
