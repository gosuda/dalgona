package inputtext

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"strconv"
	"strings"
	"testing"
	"unicode"
)

type ucdExpectation struct {
	path   string
	sha256 string
	values map[string]scalarProperties
}

var ucdExpectations = []ucdExpectation{
	{"testdata/GraphemeBreakProperty-18.0.0.txt", "0839dcb79e4ac639ecd538b1abf7c9d22e3f9dd265b7e182d33627aa4d75b45a", map[string]scalarProperties{
		"CR": {property: cr}, "LF": {property: lf}, "Control": {property: control}, "Extend": {property: extend}, "ZWJ": {property: zwj},
		"Regional_Indicator": {property: regionalIndicator}, "Prepend": {property: prepend}, "SpacingMark": {property: spacingMark},
		"L": {property: hangulL}, "V": {property: hangulV}, "T": {property: hangulT}, "LV": {property: hangulLV}, "LVT": {property: hangulLVT},
	}},
	{"testdata/emoji-data-18.0.0.txt", "80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1", map[string]scalarProperties{
		"Extended_Pictographic": {property: extendedPictographic},
	}},
	{"testdata/DerivedCoreProperties-18.0.0.txt", "09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9", map[string]scalarProperties{
		"InCB; Linker": {property: incbLinker}, "InCB; Consonant": {property: incbConsonant}, "InCB; Extend": {property: incbExtend},
		"Default_Ignorable_Code_Point": {property: defaultIgnorable},
	}},
	{"testdata/PropList-18.0.0.txt", "f438f532e8737bb8a2702126cdf9c4af5e357c58c7acf9d9eb2fc7c1a1d955d6", map[string]scalarProperties{
		"White_Space": {property: whiteSpace},
	}},
	{"testdata/WordBreakProperty-18.0.0.txt", "8dbfa17063e11084201f33c3e76d485d3b9166930c71db8e39ed1c9234171aec", map[string]scalarProperties{
		"CR": {word: wordCR}, "LF": {word: wordLF}, "Newline": {word: wordNewline}, "Extend": {word: wordExtend}, "ZWJ": {word: wordZWJ},
		"Regional_Indicator": {word: wordRegionalIndicator}, "Format": {word: wordFormat}, "Katakana": {word: wordKatakana},
		"Hebrew_Letter": {word: wordHebrewLetter}, "ALetter": {word: wordALetter}, "Single_Quote": {word: wordSingleQuote},
		"Double_Quote": {word: wordDoubleQuote}, "MidNumLet": {word: wordMidNumLet}, "MidLetter": {word: wordMidLetter},
		"MidNum": {word: wordMidNum}, "Numeric": {word: wordNumeric}, "ExtendNumLet": {word: wordExtendNumLet}, "WSegSpace": {word: wordWSegSpace},
	}},
	{"testdata/SentenceBreakProperty-18.0.0.txt", "0ac10c9d6dca3b33ac695ff0968f26356465a51626950c463b3dce8fa07e61cc", map[string]scalarProperties{
		"CR": {sentence: sentenceCR}, "LF": {sentence: sentenceLF}, "Extend": {sentence: sentenceExtend}, "Sep": {sentence: sentenceSep},
		"Format": {sentence: sentenceFormat}, "Sp": {sentence: sentenceSp}, "Lower": {sentence: sentenceLower}, "Upper": {sentence: sentenceUpper},
		"OLetter": {sentence: sentenceOLetter}, "Numeric": {sentence: sentenceNumeric}, "ATerm": {sentence: sentenceATerm},
		"SContinue": {sentence: sentenceSContinue}, "STerm": {sentence: sentenceSTerm}, "Close": {sentence: sentenceClose},
	}},
}

func TestLookupMatchesPinnedUnicodeData(t *testing.T) {
	want := make([]scalarProperties, unicode.MaxRune+1)
	for _, e := range ucdExpectations {
		readUCDExpectation(t, e, want)
	}
	mismatches := 0
	for scalar, expected := range want {
		if got := lookup(rune(scalar)); got != expected && mismatches < 20 {
			mismatches++
			t.Errorf("lookup(%U) = %+v, want %+v", scalar, got, expected)
		}
	}
}

func readUCDExpectation(t *testing.T, e ucdExpectation, want []scalarProperties) {
	t.Helper()
	data, err := os.ReadFile(e.path)
	if err != nil {
		t.Fatalf("read %s: %v", e.path, err)
	}
	sum := sha256.Sum256(data)
	if got := hex.EncodeToString(sum[:]); got != e.sha256 {
		t.Fatalf("%s sha256 = %s, want %s", e.path, got, e.sha256)
	}
	for line := range strings.Lines(string(data)) {
		content, _, _ := strings.Cut(line, "#")
		scalars, value, found := strings.Cut(content, ";")
		assigned, known := e.values[strings.Join(strings.Fields(value), " ")]
		if !found || !known {
			continue
		}
		first, last := parseUCDRange(t, strings.TrimSpace(scalars))
		for scalar := first; scalar <= last; scalar++ {
			want[scalar].property |= assigned.property
			want[scalar].word |= assigned.word
			want[scalar].sentence |= assigned.sentence
		}
	}
}

func parseUCDRange(t *testing.T, field string) (first, last int) {
	t.Helper()
	low, high, isRange := strings.Cut(field, "..")
	if !isRange {
		high = low
	}
	first64, err := strconv.ParseUint(low, 16, 32)
	if err != nil {
		t.Fatalf("code point %q: %v", field, err)
	}
	last64, err := strconv.ParseUint(high, 16, 32)
	if err != nil {
		t.Fatalf("code point %q: %v", field, err)
	}
	return int(first64), int(last64)
}
