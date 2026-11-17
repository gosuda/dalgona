package model

import (
	"math"
	"math/bits"
)

const microsPerMillionTokens = 1_000_000

type Price struct{ InputMicros, OutputMicros, CacheReadMicros, CacheWriteMicros int64 }

type TokenCounts struct{ Input, Output, CacheRead, CacheWrite int }

func (p Price) CostMicros(c TokenCounts) int64 {
	terms := [...]struct {
		tokens int
		rate   int64
	}{
		{c.Input, p.InputMicros},
		{c.Output, p.OutputMicros},
		{c.CacheRead, p.CacheReadMicros},
		{c.CacheWrite, p.CacheWriteMicros},
	}
	var high, low, carry uint64
	for _, term := range terms {
		termHigh, termLow := bits.Mul64(uint64(max(term.tokens, 0)), uint64(max(term.rate, 0)))
		low, carry = bits.Add64(low, termLow, 0)
		high, _ = bits.Add64(high, termHigh, carry)
	}
	low, carry = bits.Add64(low, microsPerMillionTokens/2, 0)
	high += carry
	if high >= microsPerMillionTokens {
		return math.MaxInt64
	}
	quotient, _ := bits.Div64(high, low, microsPerMillionTokens)
	return int64(min(quotient, math.MaxInt64))
}

func PriceFor(modelID string, configured map[string]Price) (Price, bool) {
	if price, ok := configured[modelID]; ok {
		return price, true
	}
	price, ok := presetPrices[modelID]
	return price, ok
}

var presetPrices = map[string]Price{
	"openai/gpt-6.1-sol":           {InputMicros: 2_000_000, OutputMicros: 10_000_000, CacheReadMicros: 100_000, CacheWriteMicros: 2_500_000},
	"openai/gpt-6-luna":            {InputMicros: 100_000, OutputMicros: 500_000, CacheReadMicros: 10_000, CacheWriteMicros: 125_000},
	"openai/gpt-6-astra":           {InputMicros: 10_000_000, OutputMicros: 50_000_000, CacheReadMicros: 1_000_000, CacheWriteMicros: 12_500_000},
	"anthropic/claude-opus-5-5":    {InputMicros: 4_000_000, OutputMicros: 20_000_000, CacheReadMicros: 200_000, CacheWriteMicros: 5_000_000},
	"anthropic/claude-haiku-5-5":   {InputMicros: 100_000, OutputMicros: 500_000, CacheReadMicros: 10_000, CacheWriteMicros: 125_000},
	"anthropic/claude-fable-5-1":   {InputMicros: 10_000_000, OutputMicros: 50_000_000, CacheReadMicros: 250_000, CacheWriteMicros: 12_500_000},
	"google/gemini-3.8-flash":      {InputMicros: 750_000, OutputMicros: 3_750_000, CacheReadMicros: 75_000, CacheWriteMicros: 750_000},
	"google/gemini-3.5-flash-lite": {InputMicros: 300_000, OutputMicros: 2_500_000, CacheReadMicros: 30_000, CacheWriteMicros: 300_000},
	"google/gemini-4-argon":        {InputMicros: 2_000_000, OutputMicros: 10_000_000, CacheReadMicros: 100_000, CacheWriteMicros: 2_000_000},
}
