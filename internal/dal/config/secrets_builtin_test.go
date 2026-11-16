package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestLoad_secretsBuiltin(t *testing.T) {
	cases := []struct {
		name     string
		config   string
		settings string
		want     bool
	}{
		{"default", "", "", true},
		{"config turns it off", "[secrets]\nbuiltin = false\n", "", false},
		{"settings turn it back on", "[secrets]\nbuiltin = false\n", "[secrets]\nbuiltin = true\n", true},
		{"patterns alone keep it on", "[secrets]\npatterns = [\"token-[a-z]+\"]\n", "", true},
	}
	for _, product := range []config.Product{config.ProductDal, config.ProductDalgona} {
		for _, item := range cases {
			cfg, err := loadConfig(t, product, item.config, item.settings, config.Flags{})
			if err != nil {
				t.Fatalf("%s: %v", item.name, err)
			}
			if cfg.Secrets.Builtin != item.want {
				t.Errorf("%s: Builtin = %v, want %v", item.name, cfg.Secrets.Builtin, item.want)
			}
		}
	}
}

func TestLoad_secretsBuiltinRejectsNonBoolean(t *testing.T) {
	if _, err := loadConfig(t, config.ProductDalgona, "[secrets]\nbuiltin = \"off\"\n", "", config.Flags{}); err == nil {
		t.Fatal("a string value for secrets.builtin loaded")
	}
}
