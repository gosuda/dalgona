package model

import (
	"math"
	"testing"
)

func TestCostMicros(t *testing.T) {
	t.Parallel()
	price := Price{InputMicros: 3_000_000, OutputMicros: 15_000_000, CacheReadMicros: 300_000, CacheWriteMicros: 3_750_000}
	counts := TokenCounts{Input: 1000, Output: 500, CacheRead: 2000, CacheWrite: 100}
	cases := []struct {
		name   string
		counts TokenCounts
		want   int64
	}{
		{name: "input", counts: TokenCounts{Input: counts.Input}, want: 3000},
		{name: "output", counts: TokenCounts{Output: counts.Output}, want: 7500},
		{name: "cache read", counts: TokenCounts{CacheRead: counts.CacheRead}, want: 600},
		{name: "cache write", counts: TokenCounts{CacheWrite: counts.CacheWrite}, want: 375},
		{name: "sum", counts: counts, want: 11475},
		{name: "no tokens", counts: TokenCounts{}, want: 0},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := price.CostMicros(test.counts); got != test.want {
				t.Fatalf("CostMicros(%+v) = %d, want %d", test.counts, got, test.want)
			}
		})
	}
}

func TestCostMicrosRoundsHalfUpOnce(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		price  Price
		counts TokenCounts
		want   int64
	}{
		{name: "below half", price: Price{InputMicros: 499_999}, counts: TokenCounts{Input: 1}, want: 0},
		{name: "exactly half", price: Price{InputMicros: 500_000}, counts: TokenCounts{Input: 1}, want: 1},
		{name: "above half", price: Price{InputMicros: 500_001}, counts: TokenCounts{Input: 1}, want: 1},
		{name: "two halves round together", price: Price{InputMicros: 400_000, OutputMicros: 400_000}, counts: TokenCounts{Input: 1, Output: 1}, want: 1},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := test.price.CostMicros(test.counts); got != test.want {
				t.Fatalf("CostMicros(%+v) = %d, want %d", test.counts, got, test.want)
			}
		})
	}
}

func TestCostMicrosSaturatesInsteadOfWrapping(t *testing.T) {
	t.Parallel()
	price := Price{InputMicros: 10_000_000_000, OutputMicros: 10_000_000_000}
	if got := price.CostMicros(TokenCounts{Input: math.MaxInt, Output: math.MaxInt}); got != math.MaxInt64 {
		t.Fatalf("CostMicros at the token limit = %d, want %d", got, int64(math.MaxInt64))
	}
	if got := price.CostMicros(TokenCounts{Input: 1_000_000}); got != 10_000_000_000 {
		t.Fatalf("CostMicros of one million tokens = %d, want 10000000000", got)
	}
}

func TestPriceForPrefersConfiguredPrice(t *testing.T) {
	t.Parallel()
	configured := Price{InputMicros: 1, OutputMicros: 2, CacheReadMicros: 3, CacheWriteMicros: 4}
	got, ok := PriceFor("openai/gpt-6.1-sol", map[string]Price{"openai/gpt-6.1-sol": configured})
	if !ok || got != configured {
		t.Fatalf("PriceFor with a configured price = %+v, %v, want %+v", got, ok, configured)
	}
	builtin, ok := PriceFor("openai/gpt-6.1-sol", nil)
	if !ok || builtin == configured || builtin.InputMicros != 2_000_000 {
		t.Fatalf("PriceFor without a configured price = %+v, %v, want the built-in row", builtin, ok)
	}
	other, ok := PriceFor("openai/gpt-6.1-sol", map[string]Price{"openai/other": configured})
	if !ok || other != builtin {
		t.Fatalf("PriceFor with a price for another model = %+v, %v, want %+v", other, ok, builtin)
	}
}

func TestPriceForUnknownModel(t *testing.T) {
	t.Parallel()
	if price, ok := PriceFor("openai/not-a-model", nil); ok || price != (Price{}) {
		t.Fatalf("PriceFor of an unknown model = %+v, %v, want no price", price, ok)
	}
}

func TestPresetPricesNameKnownPresetModels(t *testing.T) {
	t.Parallel()
	for id, price := range presetPrices {
		if _, ok := presetCapabilities[id]; !ok {
			t.Errorf("presetPrices has %q, which is not a preset model", id)
		}
		if price.InputMicros <= 0 || price.OutputMicros <= 0 || price.CacheReadMicros < 0 || price.CacheWriteMicros < price.InputMicros {
			t.Errorf("presetPrices[%q] = %+v is not a plausible row", id, price)
		}
	}
	if len(presetPrices) == 0 {
		t.Fatal("presetPrices holds no row")
	}
}
