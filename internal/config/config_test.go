package config_test

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

func writeConfigFile(t *testing.T, home, name, body string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(home, name), []byte(body), 0o600); err != nil {
		t.Fatalf("write %s: %v", name, err)
	}
}

func loadConfig(t *testing.T, product config.Product, configBody, settingsBody string, flags config.Flags) (config.Config, error) {
	t.Helper()
	home := t.TempDir()
	if product == config.ProductDal {
		t.Setenv("DAL_HOME", home)
	} else {
		t.Setenv("DALGONA_HOME", home)
	}
	writeConfigFile(t, home, "config.toml", configBody)
	writeConfigFile(t, home, "settings.toml", settingsBody)
	return config.Load(product, t.TempDir(), flags)
}

func requireConfigError(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func requireConfigValue[T comparable](t *testing.T, got, want T) {
	t.Helper()
	if got != want {
		t.Fatalf("value = %v, want %v", got, want)
	}
}

func TestLoad_defaults(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Model, "")
	requireConfigValue(t, cfg.Reasoning, config.ReasoningDefault)
	requireConfigValue(t, cfg.Approval, config.ApprovalAuto)
	requireConfigValue(t, cfg.ToolMode, config.ToolModeNormal)
	requireConfigValue(t, cfg.Sandbox, config.SandboxWorkspaceWrite)
	requireConfigValue(t, cfg.Runner, "local")
	requireConfigValue(t, cfg.EditStyle.EditStyle("any/model"), "anchor")
	requireConfigValue(t, cfg.StreamStartTimeout, time.Minute)
	requireConfigValue(t, cfg.StreamIdleTimeout, 5*time.Minute)
	requireConfigValue(t, cfg.ApprovalDecider.Threshold, 0.9)
}

func TestLoad_modelPrecedence(t *testing.T) {
	tests := []struct {
		name     string
		config   string
		settings string
		flags    config.Flags
		want     string
	}{
		{name: "config", config: "model = \"config/model\"\n", want: "config/model"},
		{name: "settings", config: "model = \"config/model\"\n", settings: "model = \"settings/model\"\n", want: "settings/model"},
		{name: "flags", config: "model = \"config/model\"\n", settings: "model = \"settings/model\"\n", flags: config.Flags{Model: "flag/model"}, want: "flag/model"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			cfg, err := loadConfig(t, config.ProductDalgona, test.config, test.settings, test.flags)
			if err != nil {
				t.Fatal(err)
			}
			requireConfigValue(t, cfg.Model, test.want)
		})
	}
}

func TestLoad_explicitEmptyAndFalseOverrides(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "sandbox_writable_roots = [\"/work\"]\n[herdr]\nsubagent_panes = true\n", "sandbox_writable_roots = []\n[herdr]\nsubagent_panes = false\n", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.SandboxWritableRoots) != 0 || cfg.Herdr.SubagentPanes {
		t.Fatalf("settings did not override explicit values: %+v", cfg)
	}
}

func TestLoad_unknownKeyReportsFileAndLine(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "model = \"x\"\nunknown = true\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml:2: unknown key \"unknown\"")

	_, err = loadConfig(t, config.ProductDalgona, "", "\nother = 1\n", config.Flags{})
	requireConfigError(t, err, "settings.toml:2: unknown key \"other\"")

	_, err = loadConfig(t, config.ProductDalgona, "[approval_cfg]\nthreshold = 0.5\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml:1: unknown key \"approval_cfg\"")
}

func TestLoad_approvalValidation(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "approval = \"edits\"\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml:1: approval must be one of auto, yolo, ask")
}

func TestLoad_keywordValidation(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{
		{name: "word", body: "[keywords.Review]\ntext = \"review\"\n", want: "config.toml: keyword \"Review\" must be lowercase letters, digits, _ or -, 2-32 chars"},
		{name: "missing text and notice", body: "[keywords.review]\nenabled = true\n", want: "config.toml: keyword \"review\" needs exactly one of notice or text"},
		{name: "both text and notice", body: "[keywords.review]\ntext = \"x\"\nnotice = \"n.md\"\n", want: "config.toml: keyword \"review\" needs exactly one of notice or text"},
		{name: "missing notice", body: "[keywords.review]\nnotice = \"notices/review.md\"\n", want: "config.toml: keyword \"review\" notice file notices/review.md not found"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadConfig(t, config.ProductDalgona, test.body, "", config.Flags{})
			requireConfigError(t, err, test.want)
		})
	}

	cfg, err := loadConfig(t, config.ProductDalgona, "[keywords.ultrathink]\nenabled = false\n", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Keywords["ultrathink"].Enabled {
		t.Fatal("built-in override was not retained")
	}
}

func TestLoad_validKeywordNotice(t *testing.T) {
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	if err := os.Mkdir(filepath.Join(home, "notices"), 0o700); err != nil {
		t.Fatal(err)
	}
	writeConfigFile(t, home, "notices/review.md", "Review the change.\n")
	writeConfigFile(t, home, "config.toml", "[keywords.review]\nnotice = \"notices/review.md\"\nhue = [200, 260]\n")
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Keywords["review"].Hue != [2]int{200, 260} {
		t.Fatalf("keyword = %+v", cfg.Keywords["review"])
	}
}

func TestLoad_editStyleValidation(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{
		{name: "style", body: "edit_style = \"magic\"\n", want: "config.toml: edit_style must be one of anchor, replace, hashline, hashline-light, hashline-enhanced, apply_patch, simple, balanced, strict."},
		{name: "default", body: "[edit_style]\n\"openai/*\" = \"anchor\"\n", want: "config.toml: edit_style table needs a default entry."},
		{name: "empty key", body: "[edit_style]\ndefault = \"anchor\"\n\"\" = \"replace\"\n", want: "config.toml: edit_style keys must not be empty."},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadConfig(t, config.ProductDalgona, test.body, "", config.Flags{})
			requireConfigError(t, err, test.want)
		})
	}
}

func TestEditStyleSpec_EditStyle(t *testing.T) {
	body := "[edit_style]\ndefault = \"anchor\"\n\"openai/*\" = \"replace\"\n\"openai/gpt-?\" = \"hashline\"\n"
	cfg, err := loadConfig(t, config.ProductDalgona, body, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if got := cfg.EditStyle.EditStyle("OpenAI/GPT-6"); got != "replace" {
		t.Fatalf("first source row did not win: %q", got)
	}
	if got := cfg.EditStyle.EditStyle("anthropic/claude"); got != "anchor" {
		t.Fatalf("default = %q", got)
	}

	body = "edit_style = { default = \"anchor\", \"openai/gpt-?\" = \"hashline\", \"openai/*\" = \"replace\" }\n"
	cfg, err = loadConfig(t, config.ProductDalgona, body, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if got := cfg.EditStyle.EditStyle("OPENAI/GPT-6"); got != "hashline" {
		t.Fatalf("inline first row or ? match = %q", got)
	}
	if got := cfg.EditStyle.EditStyle("openai/gpt-66"); got != "replace" {
		t.Fatalf("star match = %q", got)
	}
}

func TestLoad_approvalDecider(t *testing.T) {
	body := "approval = \"ask\"\n[approval_decider]\ndecider = \"decisions:typesafe/jev-1.14\"\nthreshold = 0.75\n"
	cfg, err := loadConfig(t, config.ProductDalgona, body, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Approval != config.ApprovalAsk || cfg.ApprovalDecider.Decider != "decisions:typesafe/jev-1.14" || cfg.ApprovalDecider.Threshold != 0.75 {
		t.Fatalf("approval config = %+v", cfg)
	}

	_, err = loadConfig(t, config.ProductDalgona, "[approval_decider]\ndecider = \"model:smol\"\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml:2: [approval_decider] decider must start with decisions: or llm:")
	_, err = loadConfig(t, config.ProductDalgona, "[approval_decider]\nthreshold = 0\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml:2: [approval_decider] threshold must be in (0, 1]")
}

func TestLoad_editReview(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.EditReview.Enabled, false)
	if cfg.EditReview.Feedback != nil {
		t.Fatalf("default feedback = %v, want nil", cfg.EditReview.Feedback)
	}

	cfg, err = loadConfig(t, config.ProductDalgona, "[edit_review]\nenabled = true\n", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.EditReview.Enabled, true)

	cfg, err = loadConfig(t, config.ProductDalgona, "[edit_review]\nenabled = true\n", "[edit_review]\nenabled = false\n", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.EditReview.Enabled, false)

	_, err = loadConfig(t, config.ProductDalgona, "[edit_review]\nenabled = \"yes\"\n", "", config.Flags{})
	if err == nil {
		t.Fatal("enabled = \"yes\" loaded, want an error")
	}
}

func TestLoad_editReviewFeedback(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "[edit_review]\nfeedback = [\"lost_code\", \"wrong_logic\"]\n", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.EditReview.Feedback) != 2 || cfg.EditReview.Feedback[0] != "lost_code" || cfg.EditReview.Feedback[1] != "wrong_logic" {
		t.Fatalf("feedback = %v, want [lost_code wrong_logic]", cfg.EditReview.Feedback)
	}

	cfg, err = loadConfig(t, config.ProductDalgona, "[edit_review]\nfeedback = []\n", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.EditReview.Feedback == nil || len(cfg.EditReview.Feedback) != 0 {
		t.Fatalf("empty feedback = %#v, want non-nil empty list", cfg.EditReview.Feedback)
	}

	_, err = loadConfig(t, config.ProductDalgona, "[edit_review]\nfeedback = \"lost_code\"\n", "", config.Flags{})
	if err == nil {
		t.Fatal("feedback as a string loaded, want an error")
	}
}

func TestLoad_dalRejectsDalgonaSettings(t *testing.T) {
	tests := []struct {
		key  string
		body string
	}{
		{key: "tool_mode", body: "tool_mode = \"normal\"\n"},
		{key: "runner", body: "runner = \"local\"\n"},
		{key: "remote", body: "remote = \"\"\n"},
		{key: "runners", body: "[runners]\n"},
		{key: "remotes", body: "[remotes]\n"},
		{key: "mcp_servers", body: "[mcp_servers]\n"},
		{key: "discovery", body: "[discovery]\n"},
		{key: "eval", body: "[eval]\nuses = []\n"},
		{key: "herdr", body: "[herdr]\nsubagent_panes = false\n"},
		{key: "edit_review", body: "[edit_review]\nenabled = false\n"},
		{key: "web", body: "[web]\nlisten = \"127.0.0.1:7437\"\n"},
	}
	for _, test := range tests {
		t.Run(test.key, func(t *testing.T) {
			_, err := loadConfig(t, config.ProductDal, test.body, "", config.Flags{})
			requireConfigError(t, err, "config.toml:1: "+test.key+" is a dalgona setting")
		})
	}
}

func TestLoad_rejectsEmptyTrustedWorkspace(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "", "mcp_trusted_workspaces = [\"\"]\n", config.Flags{})
	requireConfigError(t, err, "settings.toml: mcp_trusted_workspaces entry must be a nonempty path")
}

func TestLoad_rejectsInvalidDuration(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "stream_start_timeout = \"soon\"\n", "", config.Flags{})
	if err == nil || !strings.Contains(err.Error(), "stream_start_timeout") {
		t.Fatalf("invalid duration error = %v", err)
	}
}

func TestLoad_sandbox0RejectsImage(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "[runners.remote]\nservice = \"sandbox0\"\nimage = \"image\"\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml: runners.remote: sandbox0 takes template, not image")
}

func TestLoad_planKeys(t *testing.T) {
	body := `
model = "openai/gpt"
reasoning = "high"
approval = "auto"
tool_mode = "eval-first"
sandbox = "off"
sandbox_writable_roots = ["/tmp"]
compaction_threshold = 0.7
compaction_keep_tokens = 1000
stream_start_timeout = "2s"
stream_idle_timeout = "0s"
runner = "box"
remote = "cloud"
[providers.custom]
base_url = "https://example.test"
api_key_env = "CUSTOM_KEY"
[models."openai/gpt"]
context_window = 12345
[runners.box]
service = "docker"
image = "image"
template = "template"
url = "https://box.test"
[mcp_servers.docs]
command = "server"
args = ["--stdio"]
env = {KEY = "value"}
url = "https://mcp.test"
[discovery]
sources = ["codex"]
[eval]
uses = []
[secrets]
patterns = ["token-[a-z]+"]
[herdr]
subagent_panes = true
[web]
listen = "0.0.0.0:7437"
token = "12345678901234567890123456789012"
cert_file = "cert"
key_file = "key"
[roles]
smol = "openai/small"
slow = "openai/slow"
`
	cfg, err := loadConfig(t, config.ProductDalgona, body, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Eval.Set, true)
	requireConfigValue(t, cfg.Eval.Uses == nil, false)
	requireConfigValue(t, len(cfg.Eval.Uses), 0)
	requireConfigValue(t, cfg.Providers["custom"].APIKeyEnv, "CUSTOM_KEY")
	requireConfigValue(t, cfg.Models["openai/gpt"].ContextWindow, 12345)
	requireConfigValue(t, cfg.Runners["box"].Template, "template")
	requireConfigValue(t, cfg.MCPServers["docs"].Env["KEY"], "value")
	requireConfigValue(t, cfg.Roles.Smol, "openai/small")
	requireConfigValue(t, cfg.StreamIdleTimeout, time.Duration(0))
}

func TestLoad_missingFiles(t *testing.T) {
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Home, home)
}

func TestLoad_dalRejectsDalgonaTableSyntax(t *testing.T) {
	for _, body := range []string{"[web] # note\n", "[ web ]\n", "[\"web\"]\n"} {
		_, err := loadConfig(t, config.ProductDal, body, "", config.Flags{})
		requireConfigError(t, err, "config.toml:1: web is a dalgona setting")
	}
}

func TestLoad_dalIgnoresTableTextInsideMultilineString(t *testing.T) {
	body := `model = """openai/gpt
[web]
tool_mode = "eval-only"
"""
`
	cfg, err := loadConfig(t, config.ProductDal, body, "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.Model, "openai/gpt\n[web]\ntool_mode = \"eval-only\"\n")
}

func TestLoad_rejectsLegacyApprovalTable(t *testing.T) {
	body := "approval = \"auto\"\n[approval]\ndecider = \"llm:smol\"\n"
	_, err := loadConfig(t, config.ProductDalgona, body, "", config.Flags{})
	if err == nil {
		t.Fatal("legacy [approval] table was accepted")
	}
}

func TestWriteSetting_rejectsInvalidOutputWithoutReplacingFile(t *testing.T) {
	for _, test := range []struct {
		key   string
		value any
	}{
		{key: "wat", value: 1},
		{key: "remotes.box.wat", value: "x"},
		{key: "approval", value: "edits"},
	} {
		home := t.TempDir()
		original := []byte("model = \"settings/model\"\n")
		writeConfigFile(t, home, "settings.toml", string(original))
		if err := config.WriteSettings(home, config.SettingEdit{Key: test.key, Value: test.value}); err == nil {
			t.Fatalf("WriteSettings(%q) succeeded", test.key)
		}
		raw, err := os.ReadFile(filepath.Join(home, "settings.toml"))
		if err != nil {
			t.Fatal(err)
		}
		requireConfigValue(t, string(raw), string(original))
	}
}

func TestWriteSetting_roundTripsRemotes(t *testing.T) {
	home := t.TempDir()
	writeConfigFile(t, home, "config.toml", "[remotes.work]\nservice = \"docker\"\nimage = \"dalgona-box:dev\"\nbox_id = \"old\"\n")
	if err := config.WriteSettings(home, config.SettingEdit{Key: "remotes.work", Value: map[string]any{"box_id": "abc123"}}); err != nil {
		t.Fatal(err)
	}
	t.Setenv("DALGONA_HOME", home)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	want := config.RemoteSpec{Service: "docker", Image: "dalgona-box:dev", BoxID: "abc123"}
	if got := cfg.Remotes["work"]; got != want {
		t.Fatalf("remotes.work = %+v, want %+v", got, want)
	}
	if err = config.WriteSettings(home, config.SettingEdit{Key: "remotes.work.box_id", Value: ""}); err != nil {
		t.Fatal(err)
	}
	if cfg, err = config.Load(config.ProductDalgona, t.TempDir(), config.Flags{}); err != nil || cfg.Remotes["work"].BoxID != "" {
		t.Fatalf("cleared box_id = %q, %v", cfg.Remotes["work"].BoxID, err)
	}
}

func TestWriteSetting_preservesEditStyleOrder(t *testing.T) {
	home := t.TempDir()
	body := "[edit_style]\ndefault = \"anchor\"\n\"openai/*\" = \"replace\"\n\"openai/gpt-?\" = \"hashline\"\n"
	writeConfigFile(t, home, "settings.toml", body)
	if err := config.WriteSettings(home, config.SettingEdit{Key: "model", Value: "openai/gpt-6"}); err != nil {
		t.Fatal(err)
	}
	t.Setenv("DALGONA_HOME", home)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.EditStyle.EditStyle("openai/gpt-6"), "replace")
}

func TestWriteSetting_updatesOnlySettings(t *testing.T) {
	home := t.TempDir()
	configBody := []byte("model = \"config/model\"\n")
	if err := os.WriteFile(filepath.Join(home, "config.toml"), configBody, 0o600); err != nil {
		t.Fatal(err)
	}
	writeConfigFile(t, home, "settings.toml", "model = \"settings/model\"\n")
	if err := config.WriteSettings(home, config.SettingEdit{Key: "approval_decider.threshold", Value: 0.75}); err != nil {
		t.Fatal(err)
	}
	gotConfig, err := os.ReadFile(filepath.Join(home, "config.toml"))
	if err != nil {
		t.Fatal(err)
	}
	if string(gotConfig) != string(configBody) {
		t.Fatalf("config.toml changed: %q", gotConfig)
	}
	t.Setenv("DALGONA_HOME", home)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Model != "settings/model" || cfg.ApprovalDecider.Threshold != 0.75 {
		t.Fatalf("written settings not applied: %+v", cfg)
	}
	if _, err := os.Stat(filepath.Join(home, "settings.toml.tmp")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("temporary settings file remains: %v", err)
	}
}

func TestAnchorHomeResolvesRelativeToWorkdir(t *testing.T) {
	base := t.TempDir()
	t.Chdir(base)
	t.Setenv("DALGONA_HOME", "home")
	t.Setenv("DAL_HOME", "")
	if err := os.Unsetenv("DAL_HOME"); err != nil {
		t.Fatal(err)
	}
	if err := config.AnchorHome(); err != nil {
		t.Fatal(err)
	}
	if got := os.Getenv("DAL_HOME"); got != "" {
		t.Fatalf("unset DAL_HOME became %q", got)
	}
	anchored := os.Getenv("DALGONA_HOME")
	if want := filepath.Join(base, "home"); anchored != want {
		t.Fatalf("anchored home = %q, want %q", anchored, want)
	}
	t.Setenv("DALGONA_HOME", base)
	if err := config.AnchorHome(); err != nil {
		t.Fatal(err)
	}
	if got := os.Getenv("DALGONA_HOME"); got != base {
		t.Fatalf("absolute home changed to %q", got)
	}
}

func TestLoadRejectsRelativeHome(t *testing.T) {
	base := t.TempDir()
	t.Chdir(base)
	t.Setenv("DALGONA_HOME", "home")
	if _, err := config.Load(config.ProductDalgona, base, config.Flags{}); err == nil {
		t.Fatal("load accepted a relative home")
	}
}
