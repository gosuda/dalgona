package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestSubagentStartLimit(t *testing.T) {
	for _, product := range []config.Product{config.ProductDal, config.ProductDalgona} {
		cfg, err := loadConfig(t, product, "", "", config.Flags{})
		if err != nil {
			t.Fatal(err)
		}
		requireConfigValue(t, cfg.MaxSubagentStarts, 32)
		cfg, err = loadConfig(t, product, "[limits]\nmax_subagent_starts = 2\n", "", config.Flags{})
		if err != nil {
			t.Fatal(err)
		}
		requireConfigValue(t, cfg.MaxSubagentStarts, 2)
		_, err = loadConfig(t, product, "[limits]\nmax_subagent_starts = 0\n", "", config.Flags{})
		requireConfigError(t, err, "config.toml:2: limits.max_subagent_starts must be positive")
	}
}
