package config

import (
	"cmp"
	"maps"
	"math"
	"slices"

	"github.com/gosuda/dalgona/internal/model"
)

const (
	maxUSDPerMTok = 10000
	microsPerUSD  = 1_000_000
)

type priceKeys struct{ input, output, cacheRead, cacheWrite *float64 }

type priceItem struct {
	key   string
	value *float64
}

func priceKeysOf(doc modelDoc) priceKeys {
	return priceKeys{
		input: doc.InputUSDPerMTok, output: doc.OutputUSDPerMTok,
		cacheRead: doc.CacheReadUSDPerMTok, cacheWrite: doc.CacheWriteUSDPerMTok,
	}
}

func (p priceKeys) overlay(next priceKeys) priceKeys {
	return priceKeys{
		input: cmp.Or(next.input, p.input), output: cmp.Or(next.output, p.output),
		cacheRead: cmp.Or(next.cacheRead, p.cacheRead), cacheWrite: cmp.Or(next.cacheWrite, p.cacheWrite),
	}
}

func (p priceKeys) items() []priceItem {
	return []priceItem{
		{"input_usd_per_mtok", p.input},
		{"output_usd_per_mtok", p.output},
		{"cache_read_usd_per_mtok", p.cacheRead},
		{"cache_write_usd_per_mtok", p.cacheWrite},
	}
}

func (p priceKeys) isSet() bool {
	return slices.ContainsFunc(p.items(), func(item priceItem) bool { return item.value != nil })
}

func (c *Config) mergeModels(doc *fileDoc) {
	for name, value := range doc.Models {
		spec := c.Models[name]
		if value.ContextWindow != nil {
			spec.ContextWindow = *value.ContextWindow
		}
		c.Models[name] = spec
		if keys := priceKeysOf(value); keys.isSet() {
			c.priceEntries[name] = c.priceEntries[name].overlay(keys)
		}
	}
}

func (c *Config) validatePrices() []error {
	var errs []error
	for _, name := range slices.Sorted(maps.Keys(c.priceEntries)) {
		errs = append(errs, c.validatePriceEntry(name, c.priceEntries[name])...)
	}
	return errs
}

func (c *Config) validatePriceEntry(name string, keys priceKeys) []error {
	table := "models." + name
	label := `[models."` + name + `"] `
	var errs []error
	for _, item := range keys.items() {
		if item.value != nil && !validUSDPerMTok(*item.value) {
			errs = append(errs, c.valueError(table+"."+item.key, label+item.key+" must be between 0 and 10000"))
		}
	}
	if keys.input == nil || keys.output == nil {
		errs = append(errs, c.valueError(table, label+"input_usd_per_mtok and output_usd_per_mtok must be set together"))
	}
	return errs
}

func validUSDPerMTok(value float64) bool {
	return value >= 0 && value <= maxUSDPerMTok
}

func (c *Config) resolvePrices() {
	for name, keys := range c.priceEntries {
		input := usdToMicros(*keys.input)
		price := model.Price{
			InputMicros: input, OutputMicros: usdToMicros(*keys.output),
			CacheReadMicros: input, CacheWriteMicros: input,
		}
		if keys.cacheRead != nil {
			price.CacheReadMicros = usdToMicros(*keys.cacheRead)
		}
		if keys.cacheWrite != nil {
			price.CacheWriteMicros = usdToMicros(*keys.cacheWrite)
		}
		spec := c.Models[name]
		spec.Price = &price
		c.Models[name] = spec
	}
}

func usdToMicros(usd float64) int64 {
	return int64(math.Round(usd * microsPerUSD))
}
