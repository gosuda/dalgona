package config_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestLoad_failureReportsDefaults(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.FailureReports.Enabled, false)
	requireConfigValue(t, cfg.FailureReports.Auth, "app")
}

func TestLoad_failureReportsMerge(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDalgona, "[failure_reports]\nenabled = true\nauth = \"gh\"\n", "", config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	requireConfigValue(t, cfg.FailureReports.Enabled, true)
	requireConfigValue(t, cfg.FailureReports.Auth, "gh")
}

func TestLoad_failureReportsInvalidAuth(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "[failure_reports]\nauth = \"token\"\n", "", config.Flags{})
	if err == nil || !strings.Contains(err.Error(), "config: failure_reports.auth must be one of app, gh") {
		t.Fatalf("error = %v, want failure_reports.auth enum error", err)
	}
}

func TestLoad_failureReportsDalgonaOnly(t *testing.T) {
	_, err := loadConfig(t, config.ProductDal, "[failure_reports]\nenabled = true\n", "", config.Flags{})
	if err == nil {
		t.Fatal("expected failure_reports to be rejected under dal")
	}
}
