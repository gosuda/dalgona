//go:build ignore

package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"strconv"
	"strings"
)

const (
	maxScalar  = 0x10ffff
	outputPath = "web/src/lib/unicode-properties.ts"
	classMask  = 0x0f
	incbMask   = 3 << 5
)

type ucdFile struct {
	path   string
	sha256 string
	values map[string]uint16
}

var ucdFiles = []ucdFile{
	{"internal/dal/inputtext/testdata/GraphemeBreakProperty-18.0.0.txt", "0839dcb79e4ac639ecd538b1abf7c9d22e3f9dd265b7e182d33627aa4d75b45a", map[string]uint16{
		"CR": 1, "LF": 2, "Control": 3, "Extend": 4, "ZWJ": 5, "Regional_Indicator": 6, "Prepend": 7,
		"SpacingMark": 8, "L": 9, "V": 10, "T": 11, "LV": 12, "LVT": 13,
	}},
	{"internal/dal/inputtext/testdata/emoji-data-18.0.0.txt", "80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1", map[string]uint16{
		"Extended_Pictographic": 1 << 4,
	}},
	{"internal/dal/inputtext/testdata/DerivedCoreProperties-18.0.0.txt", "09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9", map[string]uint16{
		"InCB; Linker": 1 << 5, "InCB; Consonant": 2 << 5, "InCB; Extend": 3 << 5, "Default_Ignorable_Code_Point": 1 << 8,
	}},
	{"internal/dal/inputtext/testdata/PropList-18.0.0.txt", "f438f532e8737bb8a2702126cdf9c4af5e357c58c7acf9d9eb2fc7c1a1d955d6", map[string]uint16{
		"White_Space": 1 << 7,
	}},
}

func main() {
	if err := generate(); err != nil {
		fmt.Fprintln(os.Stderr, "unicodeprops:", err)
		os.Exit(1)
	}
}

func generate() error {
	properties := make([]uint16, maxScalar+1)
	for _, file := range ucdFiles {
		if err := apply(file, properties); err != nil {
			return err
		}
	}
	if err := os.WriteFile(outputPath, render(properties), 0o644); err != nil {
		return fmt.Errorf("write %s: %w", outputPath, err)
	}
	return nil
}

func apply(file ucdFile, properties []uint16) error {
	data, err := os.ReadFile(file.path)
	if err != nil {
		return fmt.Errorf("read %s: %w", file.path, err)
	}
	sum := sha256.Sum256(data)
	if got := hex.EncodeToString(sum[:]); got != file.sha256 {
		return fmt.Errorf("%s has sha256 %s, want %s", file.path, got, file.sha256)
	}
	for number, line := range strings.Split(string(data), "\n") {
		if err := applyLine(file, line, properties); err != nil {
			return fmt.Errorf("%s:%d: %w", file.path, number+1, err)
		}
	}
	return nil
}

func applyLine(file ucdFile, line string, properties []uint16) error {
	content, _, _ := strings.Cut(line, "#")
	scalars, value, found := strings.Cut(content, ";")
	bit, known := file.values[strings.Join(strings.Fields(value), " ")]
	if !found || !known {
		return nil
	}
	first, last, err := parseRange(strings.TrimSpace(scalars))
	if err != nil {
		return err
	}
	for scalar := first; scalar <= last; scalar++ {
		if overlaps(properties[scalar], bit) {
			return fmt.Errorf("U+%04X has two values for one property", scalar)
		}
		properties[scalar] |= bit
	}
	return nil
}

func overlaps(current, bit uint16) bool {
	for _, field := range [...]uint16{classMask, incbMask} {
		if bit&field != 0 && current&field != 0 {
			return true
		}
	}
	return current&bit != 0
}

func parseRange(field string) (int, int, error) {
	low, high, isRange := strings.Cut(field, "..")
	if !isRange {
		high = low
	}
	first, err := strconv.ParseUint(low, 16, 32)
	if err != nil {
		return 0, 0, fmt.Errorf("code point %q: %w", field, err)
	}
	last, err := strconv.ParseUint(high, 16, 32)
	if err != nil {
		return 0, 0, fmt.Errorf("code point %q: %w", field, err)
	}
	if first > last || last > maxScalar {
		return 0, 0, fmt.Errorf("code point range %q is out of order or above U+10FFFF", field)
	}
	return int(first), int(last), nil
}

func render(properties []uint16) []byte {
	runs := make([]string, 0, 4096)
	previous := 0
	for first := 0; first <= maxScalar; {
		last := first
		for last < maxScalar && properties[last+1] == properties[first] {
			last++
		}
		runs = append(runs, strconv.FormatInt(int64(first-previous), 36)+"."+strconv.FormatInt(int64(properties[first]), 36))
		previous = first
		first = last + 1
	}
	return fmt.Appendf(nil, "export const propertyRuns: string =\n\t%q;\n", strings.Join(runs, ","))
}
