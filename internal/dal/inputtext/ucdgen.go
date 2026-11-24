//go:build ignore

package main

import (
	"bytes"
	"cmp"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"go/format"
	"maps"
	"math/bits"
	"os"
	"slices"
	"strconv"
	"strings"
)

const (
	maxScalar      = 0x10ffff
	scalarBits     = 21
	outputPath     = "property_table.go"
	runsPerLine    = 8
	derivedPath    = "testdata/DerivedCoreProperties-18.0.0.txt"
	derivedSHA256  = "09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9"
	graphemeField  = 0
	wordField      = 1
	sentenceField  = 2
	propertyFields = 3
)

type ucdProperty struct {
	path   string
	sha256 string
	field  int
	values map[string]string
}

var ucdProperties = [...]ucdProperty{
	{"testdata/GraphemeBreakProperty-18.0.0.txt", "0839dcb79e4ac639ecd538b1abf7c9d22e3f9dd265b7e182d33627aa4d75b45a", graphemeField, map[string]string{
		"CR": "cr", "LF": "lf", "Control": "control", "Extend": "extend", "ZWJ": "zwj", "Regional_Indicator": "regionalIndicator",
		"Prepend": "prepend", "SpacingMark": "spacingMark", "L": "hangulL", "V": "hangulV", "T": "hangulT", "LV": "hangulLV", "LVT": "hangulLVT",
	}},
	{"testdata/emoji-data-18.0.0.txt", "80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1", graphemeField, map[string]string{
		"Extended_Pictographic": "extendedPictographic",
	}},
	{derivedPath, derivedSHA256, graphemeField, map[string]string{
		"InCB; Linker": "incbLinker", "InCB; Consonant": "incbConsonant", "InCB; Extend": "incbExtend",
	}},
	{"testdata/PropList-18.0.0.txt", "f438f532e8737bb8a2702126cdf9c4af5e357c58c7acf9d9eb2fc7c1a1d955d6", graphemeField, map[string]string{
		"White_Space": "whiteSpace",
	}},
	{derivedPath, derivedSHA256, graphemeField, map[string]string{
		"Default_Ignorable_Code_Point": "defaultIgnorable",
	}},
	{"testdata/WordBreakProperty-18.0.0.txt", "8dbfa17063e11084201f33c3e76d485d3b9166930c71db8e39ed1c9234171aec", wordField, map[string]string{
		"CR": "wordCR", "LF": "wordLF", "Newline": "wordNewline", "Extend": "wordExtend", "ZWJ": "wordZWJ",
		"Regional_Indicator": "wordRegionalIndicator", "Format": "wordFormat", "Katakana": "wordKatakana",
		"Hebrew_Letter": "wordHebrewLetter", "ALetter": "wordALetter", "Single_Quote": "wordSingleQuote",
		"Double_Quote": "wordDoubleQuote", "MidNumLet": "wordMidNumLet", "MidLetter": "wordMidLetter", "MidNum": "wordMidNum",
		"Numeric": "wordNumeric", "ExtendNumLet": "wordExtendNumLet", "WSegSpace": "wordWSegSpace",
	}},
	{"testdata/SentenceBreakProperty-18.0.0.txt", "0ac10c9d6dca3b33ac695ff0968f26356465a51626950c463b3dce8fa07e61cc", sentenceField, map[string]string{
		"CR": "sentenceCR", "LF": "sentenceLF", "Extend": "sentenceExtend", "Sep": "sentenceSep", "Format": "sentenceFormat",
		"Sp": "sentenceSp", "Lower": "sentenceLower", "Upper": "sentenceUpper", "OLetter": "sentenceOLetter",
		"Numeric": "sentenceNumeric", "ATerm": "sentenceATerm", "SContinue": "sentenceSContinue", "STerm": "sentenceSTerm",
		"Close": "sentenceClose",
	}},
}

type scalarValues [len(ucdProperties)]uint8

type run struct {
	first, palette int
}

func main() {
	if err := generate(); err != nil {
		fmt.Fprintln(os.Stderr, "ucdgen:", err)
		os.Exit(1)
	}
}

func generate() error {
	scalars := make([]scalarValues, maxScalar+1)
	var names [len(ucdProperties)][]string
	for k, property := range ucdProperties {
		names[k] = slices.Sorted(maps.Values(property.values))
		if err := apply(k, names[k], scalars); err != nil {
			return err
		}
	}
	runs, palette := compress(scalars)
	paletteBits := bits.Len(uint(len(palette) - 1))
	if scalarBits+paletteBits > 31 {
		return fmt.Errorf("%d property combinations do not fit beside %d scalar bits in an int32", len(palette), scalarBits)
	}
	source, err := format.Source(render(runs, palette, names[:], paletteBits))
	if err != nil {
		return fmt.Errorf("format %s: %w", outputPath, err)
	}
	if err := os.WriteFile(outputPath, source, 0o644); err != nil {
		return fmt.Errorf("write %s: %w", outputPath, err)
	}
	return nil
}

func apply(k int, names []string, scalars []scalarValues) error {
	property := ucdProperties[k]
	data, err := os.ReadFile(property.path)
	if err != nil {
		return fmt.Errorf("read %s: %w", property.path, err)
	}
	sum := sha256.Sum256(data)
	if got := hex.EncodeToString(sum[:]); got != property.sha256 {
		return fmt.Errorf("%s has sha256 %s, want %s", property.path, got, property.sha256)
	}
	for number, line := range strings.Split(string(data), "\n") {
		if err := applyLine(k, names, line, scalars); err != nil {
			return fmt.Errorf("%s:%d: %w", property.path, number+1, err)
		}
	}
	return nil
}

func applyLine(k int, names []string, line string, scalars []scalarValues) error {
	content, _, _ := strings.Cut(line, "#")
	fields, value, found := strings.Cut(content, ";")
	name, known := ucdProperties[k].values[strings.Join(strings.Fields(value), " ")]
	if !found || !known {
		return nil
	}
	first, last, err := parseRange(strings.TrimSpace(fields))
	if err != nil {
		return err
	}
	index := uint8(slices.Index(names, name) + 1)
	for scalar := first; scalar <= last; scalar++ {
		if scalars[scalar][k] != 0 {
			return fmt.Errorf("U+%04X has two values for one property", scalar)
		}
		scalars[scalar][k] = index
	}
	return nil
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

func compress(scalars []scalarValues) ([]run, []scalarValues) {
	positions := map[scalarValues]int{}
	var palette []scalarValues
	var runs []run
	for scalar, values := range scalars {
		if scalar > 0 && values == scalars[scalar-1] {
			continue
		}
		position, seen := positions[values]
		if !seen {
			position = len(palette)
			positions[values] = position
			palette = append(palette, values)
		}
		runs = append(runs, run{scalar, position})
	}
	return runs, palette
}

func render(runs []run, palette []scalarValues, names [][]string, paletteBits int) []byte {
	var out bytes.Buffer
	fmt.Fprintf(&out, "package inputtext\n\nconst paletteBits = %d\n\nvar scalarRuns = [...]int32{", paletteBits)
	for i, r := range runs {
		if i%runsPerLine == 0 {
			out.WriteString("\n")
		}
		fmt.Fprintf(&out, "0x%07X, ", r.first<<paletteBits|r.palette)
	}
	out.WriteString("\n}\n\nvar scalarPalette = [...]scalarProperties{\n")
	for _, values := range palette {
		fmt.Fprintf(&out, "{%s},\n", expression(values, names))
	}
	out.WriteString("}\n")
	return out.Bytes()
}

func expression(values scalarValues, names [][]string) string {
	var fields [propertyFields][]string
	for k, index := range values {
		if index != 0 {
			field := ucdProperties[k].field
			fields[field] = append(fields[field], names[k][index-1])
		}
	}
	parts := make([]string, propertyFields)
	for field, identifiers := range fields {
		parts[field] = cmp.Or(strings.Join(identifiers, " | "), "0")
	}
	return strings.Join(parts, ", ")
}
