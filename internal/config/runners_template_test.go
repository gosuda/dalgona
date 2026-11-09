package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestLoad_sandbox0RequiresTemplate(t *testing.T) {
	_, err := loadConfig(t, config.ProductDalgona, "[runners.box]\nservice = \"sandbox0\"\n", "", config.Flags{})
	requireConfigError(t, err, "config.toml: runners.box: sandbox0 requires a template")
}
