package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestLoad_advisorEntrySubagentsParsesLayersAndDefaults(t *testing.T) {
	configBody := "[advisors.force]\nsubagents = true\n" +
		"[advisors.off]\nsubagents = false\n" +
		"[advisors.plain]\nmodel = \"slow\"\n"
	settingsBody := "[advisors.force]\nsubagents = false\n"
	cfg, err := loadConfig(t, config.ProductDalgona, configBody, settingsBody, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if got := cfg.Advisors["force"].Subagents; got != config.AdvisorSubagentsOff {
		t.Fatalf("force.Subagents = %v, want off after later layer set false", got)
	}
	if got := cfg.Advisors["off"].Subagents; got != config.AdvisorSubagentsOff {
		t.Fatalf("off.Subagents = %v, want off", got)
	}
	if got := cfg.Advisors["plain"].Subagents; got != config.AdvisorSubagentsInherit {
		t.Fatalf("plain.Subagents = %v, want inherit", got)
	}
}

func TestLoad_advisorEntrySubagentsKeepsEarlierTrue(t *testing.T) {
	configBody := "[advisors.force]\nsubagents = true\n"
	settingsBody := "[advisors.force]\ninterval = 4\n"
	cfg, err := loadConfig(t, config.ProductDalgona, configBody, settingsBody, config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if got := cfg.Advisors["force"].Subagents; got != config.AdvisorSubagentsOn {
		t.Fatalf("force.Subagents = %v, want on kept from earlier layer", got)
	}
}

func TestLoad_advisorDefaultSubagentsIsInherit(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	if got := cfg.Advisors[config.DefaultAdvisorName].Subagents; got != config.AdvisorSubagentsInherit {
		t.Fatalf("default.Subagents = %v, want inherit", got)
	}
}
