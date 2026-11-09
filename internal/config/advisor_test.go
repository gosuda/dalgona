package config_test

import (
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestLoad_advisorDefaults(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	want := config.AdvisorConfig{
		Review: config.AdvisorReviewTurn, Interval: 1, Sync: config.AdvisorSyncOff, ContinueSeverity: "blocker",
		ImmuneTurns: 3, MaxNotes: 3, Tools: []string{"read", "search"},
	}
	requireDeepEqual(t, cfg.Advisor, want)
	if len(cfg.Advisors) != 1 {
		t.Fatalf("roster = %v, want only the default advisor", cfg.Advisors)
	}
	requireDeepEqual(t, cfg.Advisors[config.DefaultAdvisorName], config.AdvisorSpec{
		Review: config.AdvisorReviewTurn, Interval: 1, Sync: config.AdvisorSyncOff, Tools: []string{"read", "search"}, Enabled: true,
	})
}

func TestLoad_advisorRosterInheritsAndOverridesAcrossLayers(t *testing.T) {
	configBody := "[advisor]\nenabled = true\nreview = \"agent-end\"\ntools = []\nsubagents = true\n" +
		"[advisors.security]\nmodel = \"slow\"\ninstructions = \"Check auth.\"\nsync = \"final\"\n" +
		"[advisors.style]\nenabled = false\ntools = [\"read\"]\n"
	settingsBody := "[advisor]\ninterval = 2\nmax_notes = 0\n[advisors.security]\ninterval = 4\n"
	cfg, err := loadConfig(t, config.ProductDalgona, configBody, settingsBody, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if !cfg.Advisor.Enabled || !cfg.Advisor.Subagents || cfg.Advisor.MaxNotes != 0 || cfg.Advisor.Interval != 2 {
		t.Fatalf("advisor = %+v", cfg.Advisor)
	}
	if _, ok := cfg.Advisors[config.DefaultAdvisorName]; ok || len(cfg.Advisors) != 2 {
		t.Fatalf("roster = %v, want security and style only", cfg.Advisors)
	}
	requireDeepEqual(t, cfg.Advisors["security"], config.AdvisorSpec{
		Model: "slow", Instructions: "Check auth.", Review: config.AdvisorReviewAgentEnd, Interval: 4,
		Sync: config.AdvisorSyncFinal, Tools: []string{}, Enabled: true,
	})
	requireDeepEqual(t, cfg.Advisors["style"], config.AdvisorSpec{
		Review: config.AdvisorReviewAgentEnd, Interval: 2, Sync: config.AdvisorSyncOff, Tools: []string{"read"},
	})
}

func TestLoad_advisorFlagEnablesForOneRun(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "[advisor]\nenabled = false\n", "", config.Flags{Advisor: true})
	if err != nil {
		t.Fatal(err)
	}
	if !cfg.Advisor.Enabled {
		t.Fatal("--advisor did not enable the advisor")
	}
}

func TestLoad_dalAcceptsAdvisorTables(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDal, "[advisor]\nenabled = true\n[advisors.reviewer]\nmodel = \"anthropic/claude-fable-5-1\"\n", "[roles]\nadvisor = \"openai/gpt-6-astra\"\n", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if !cfg.Advisor.Enabled || cfg.Advisors["reviewer"].Model != "anthropic/claude-fable-5-1" || cfg.Roles.Advisor != "openai/gpt-6-astra" {
		t.Fatalf("dal advisor config = %+v %+v %+v", cfg.Advisor, cfg.Advisors, cfg.Roles)
	}
}

func TestLoad_advisorValidation(t *testing.T) {
	tests := []struct {
		name, config, settings, want string
	}{
		{name: "review", config: "[advisor]\nreview = \"step\"\n", want: "config.toml:2: [advisor] review must be one of turn, agent-end"},
		{name: "interval", config: "[advisor]\ninterval = 0\n", want: "config.toml:2: [advisor] interval must be at least 1"},
		{name: "sync", config: "[advisor]\nsync = \"always\"\n", want: "config.toml:2: [advisor] sync must be one of off, final, strict"},
		{name: "continue severity", config: "[advisor]\ncontinue_severity = \"warn\"\n", want: "config.toml:2: [advisor] continue_severity must be one of nit, concern, blocker"},
		{name: "immune turns", config: "[advisor]\nimmune_turns = -1\n", want: "config.toml:2: [advisor] immune_turns must not be negative"},
		{name: "max notes", config: "[advisor]\nmax_notes = -1\n", want: "config.toml:2: [advisor] max_notes must not be negative"},
		{name: "settings location", settings: "[advisor]\n\nreview = \"never\"\n", want: "settings.toml:3: [advisor] review must be one of turn, agent-end"},
		{name: "entry name", config: "[advisors.Bad]\nmodel = \"slow\"\n", want: "config.toml:1: [advisors.Bad] name must be lowercase letters, digits, _ or -, 1-32 chars"},
		{name: "entry model", config: "[advisors.sec]\nmodel = \"fast\"\n", want: "config.toml:2: [advisors.sec] model must be smol, default, slow, advisor, or provider/model"},
		{name: "entry half model id", config: "[advisors.sec]\nmodel = \"openai/\"\n", want: "config.toml:2: [advisors.sec] model must be smol, default, slow, advisor, or provider/model"},
		{
			name: "entry fields", config: "[advisors.sec]\nsync = \"x\"\ninterval = 0\nreview = \"y\"\n",
			want: "config.toml:4: [advisors.sec] review must be one of turn, agent-end\nconfig.toml:3: [advisors.sec] interval must be at least 1\nconfig.toml:2: [advisors.sec] sync must be one of off, final, strict",
		},
		{
			name: "base and entry", config: "[advisor]\nsync = \"x\"\n[advisors.b]\nreview = \"y\"\n[advisors.a]\ninterval = -2\n",
			want: "config.toml:2: [advisor] sync must be one of off, final, strict\nconfig.toml:6: [advisors.a] interval must be at least 1\nconfig.toml:4: [advisors.b] review must be one of turn, agent-end",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadConfig(t, config.ProductDalgona, test.config, test.settings, config.Flags{})
			requireConfigError(t, err, test.want)
		})
	}
}

func TestRolesConfig_Configured(t *testing.T) {
	tests := []struct {
		name  string
		roles config.RolesConfig
		role  string
		want  string
	}{
		{name: "advisor set", roles: config.RolesConfig{Slow: "a/slow", Advisor: "a/advisor"}, role: "advisor", want: "a/advisor"},
		{name: "advisor falls back to slow", roles: config.RolesConfig{Slow: "a/slow"}, role: "advisor", want: "a/slow"},
		{name: "advisor unset", roles: config.RolesConfig{Smol: "a/smol"}, role: "advisor", want: ""},
		{name: "smol", roles: config.RolesConfig{Smol: "a/smol", Advisor: "a/advisor"}, role: "smol", want: "a/smol"},
		{name: "slow ignores advisor", roles: config.RolesConfig{Advisor: "a/advisor"}, role: "slow", want: ""},
		{name: "default is not a configured role", roles: config.RolesConfig{Smol: "a/smol", Slow: "a/slow"}, role: "default", want: ""},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			requireConfigValue(t, test.roles.Configured(test.role), test.want)
		})
	}
}

func TestWriteSetting_togglesAdvisor(t *testing.T) {
	home := t.TempDir()
	if err := config.WriteSetting(home, "advisor.enabled", true); err != nil {
		t.Fatal(err)
	}
	t.Setenv("DALGONA_HOME", home)
	cfg, err := config.Load(config.ProductDalgona, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if !cfg.Advisor.Enabled {
		t.Fatal("advisor.enabled setting not applied")
	}
	if err := config.WriteSetting(home, "advisor.sync", "sometimes"); err == nil {
		t.Fatal("invalid advisor.sync was written")
	}
}

func requireDeepEqual[T any](t *testing.T, got, want T) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("value = %#v, want %#v", got, want)
	}
}
