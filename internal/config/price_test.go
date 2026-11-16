package config_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
)

func TestConfigPriceErrors(t *testing.T) {
	const together = `[models."openai/gpt"] input_usd_per_mtok and output_usd_per_mtok must be set together`
	tests := []struct {
		name, config, settings, want string
	}{
		{name: "input above range", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 10001\noutput_usd_per_mtok = 1\n", want: `config.toml:2: [models."openai/gpt"] input_usd_per_mtok must be between 0 and 10000`},
		{name: "output below range", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = -0.5\n", want: `config.toml:3: [models."openai/gpt"] output_usd_per_mtok must be between 0 and 10000`},
		{name: "cache read above range", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = 1\ncache_read_usd_per_mtok = 20000\n", want: `config.toml:4: [models."openai/gpt"] cache_read_usd_per_mtok must be between 0 and 10000`},
		{name: "cache write below range", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = 1\ncache_write_usd_per_mtok = -1\n", want: `config.toml:4: [models."openai/gpt"] cache_write_usd_per_mtok must be between 0 and 10000`},
		{name: "not a number", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = nan\noutput_usd_per_mtok = 1\n", want: `config.toml:2: [models."openai/gpt"] input_usd_per_mtok must be between 0 and 10000`},
		{name: "infinite", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = inf\n", want: `config.toml:3: [models."openai/gpt"] output_usd_per_mtok must be between 0 and 10000`},
		{name: "input alone", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\n", want: "config.toml:1: " + together},
		{name: "output alone", config: "[models.\"openai/gpt\"]\noutput_usd_per_mtok = 1\n", want: "config.toml:1: " + together},
		{name: "cache read alone", config: "[models.\"openai/gpt\"]\ncache_read_usd_per_mtok = 1\n", want: "config.toml:1: " + together},
		{name: "cache write with input only", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\ncache_write_usd_per_mtok = 1\n", want: "config.toml:1: " + together},
		{name: "settings location", config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = 1\n", settings: "[models.\"openai/gpt\"]\n\ninput_usd_per_mtok = 10001\n", want: `settings.toml:3: [models."openai/gpt"] input_usd_per_mtok must be between 0 and 10000`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			_, err := loadConfig(t, config.ProductDalgona, test.config, test.settings, config.Flags{})
			requireConfigError(t, err, test.want)
		})
	}
}

func TestConfigPriceAcceptsBoundariesAndLoadsMicros(t *testing.T) {
	tests := []struct {
		name, config, settings string
		want                   *model.Price
	}{
		{
			name:   "plan example",
			config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 3.00\noutput_usd_per_mtok = 15.00\ncache_read_usd_per_mtok = 0.30\ncache_write_usd_per_mtok = 3.75\n",
			want:   &model.Price{InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 300_000, CacheWriteMicros: 3_750_000},
		},
		{
			name:   "absent cache keys use the input rate",
			config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 3\noutput_usd_per_mtok = 15\n",
			want:   &model.Price{InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 3_000_000, CacheWriteMicros: 3_000_000},
		},
		{
			name:   "one cache key keeps the other at the input rate",
			config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 3\noutput_usd_per_mtok = 15\ncache_read_usd_per_mtok = 0\n",
			want:   &model.Price{InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 0, CacheWriteMicros: 3_000_000},
		},
		{
			name:   "bounds",
			config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 0\noutput_usd_per_mtok = 10000\n",
			want:   &model.Price{InputMicros: 0, OutputMicros: 10_000_000_000, CacheReadMicros: 0, CacheWriteMicros: 0},
		},
		{
			name:   "round below half a micro",
			config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 3.0000004\noutput_usd_per_mtok = 1\n",
			want:   &model.Price{InputMicros: 3_000_000, OutputMicros: 1_000_000, CacheReadMicros: 3_000_000, CacheWriteMicros: 3_000_000},
		},
		{
			name:   "round above half a micro",
			config: "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 3.0000006\noutput_usd_per_mtok = 1\n",
			want:   &model.Price{InputMicros: 3_000_001, OutputMicros: 1_000_000, CacheReadMicros: 3_000_001, CacheWriteMicros: 3_000_001},
		},
		{
			name:     "settings add a cache key to the config prices",
			config:   "[models.\"openai/gpt\"]\ninput_usd_per_mtok = 3\noutput_usd_per_mtok = 15\ncache_write_usd_per_mtok = 4\n",
			settings: "[models.\"openai/gpt\"]\ncache_read_usd_per_mtok = 0.5\n",
			want:     &model.Price{InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 500_000, CacheWriteMicros: 4_000_000},
		},
		{
			name:   "context window alone sets no price",
			config: "[models.\"openai/gpt\"]\ncontext_window = 5000\n",
			want:   nil,
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			cfg, err := loadConfig(t, config.ProductDalgona, test.config, test.settings, config.Flags{})
			if err != nil {
				t.Fatal(err)
			}
			got := cfg.Models["openai/gpt"].Price
			switch {
			case test.want == nil && got != nil:
				t.Fatalf("price = %+v, want none", *got)
			case test.want != nil && (got == nil || *got != *test.want):
				t.Fatalf("price = %v, want %+v", got, *test.want)
			}
		})
	}
}

func TestConfigPriceKeepsContextWindowAcrossFiles(t *testing.T) {
	cfg, err := loadConfig(t, config.ProductDal,
		"[models.\"openai/gpt\"]\ncontext_window = 5000\n",
		"[models.\"openai/gpt\"]\ninput_usd_per_mtok = 1\noutput_usd_per_mtok = 2\n",
		config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	spec := cfg.Models["openai/gpt"]
	requireConfigValue(t, spec.ContextWindow, 5000)
	if spec.Price == nil || spec.Price.InputMicros != 1_000_000 || spec.Price.OutputMicros != 2_000_000 {
		t.Fatalf("price = %v, want input 1000000 and output 2000000", spec.Price)
	}
}
