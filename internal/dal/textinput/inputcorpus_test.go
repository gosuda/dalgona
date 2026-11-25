package textinput

import (
	"encoding/hex"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"os"
	"slices"
	"strconv"
	"strings"
	"testing"
	"unicode"
)

const (
	corpusPath          = "../../../docs/dal/spec/input-text-cases.json"
	corpusValidCount    = 36
	corpusInvalidCount  = 9
	jsonRPCParseError   = -32700
	pinnedUnicode       = "18.0.0"
	pinnedSegmentation  = "UAX29-revision-49-default-extended-graphemes"
	scalarBoundaryUnit  = "unicode_scalar_index"
	corpusFieldLimitMax = 1 << 20
)

type inputCorpus struct {
	SchemaVersion    int            `json:"schemaVersion"`
	UnicodeVersion   string         `json:"unicodeVersion"`
	Segmentation     string         `json:"segmentation"`
	BoundaryUnit     string         `json:"boundaryUnit"`
	Vectors          []inputVector  `json:"vectors"`
	InvalidEncodings []inputInvalid `json:"invalidEncodings"`
}

type inputVector struct {
	ID                 string   `json:"id"`
	CodePoints         []string `json:"codePoints"`
	Text               string   `json:"text"`
	ScalarCount        int      `json:"scalarCount"`
	UTF8Bytes          int      `json:"utf8Bytes"`
	UTF16Units         int      `json:"utf16Units"`
	GraphemeBoundaries []int    `json:"graphemeBoundaries"`
	Policy             string   `json:"policy"`
}

type inputInvalid struct {
	ID           string `json:"id"`
	Encoding     string `json:"encoding"`
	Input        string `json:"input"`
	Reason       string `json:"reason"`
	RPCErrorCode int    `json:"rpcErrorCode"`
}

var corpusField = Policy{Lines: MultiLine, Limit: Limit{Unit: Bytes, Max: corpusFieldLimitMax}}

func loadInputCorpus(t *testing.T) inputCorpus {
	t.Helper()
	data, err := os.ReadFile(corpusPath)
	if err != nil {
		t.Fatalf("read %s: %v", corpusPath, err)
	}
	var c inputCorpus
	if err := json.Unmarshal(data, &c, json.RejectUnknownMembers(true)); err != nil {
		t.Fatalf("decode %s: %v", corpusPath, err)
	}
	if c.SchemaVersion != 1 || c.UnicodeVersion != pinnedUnicode || c.Segmentation != pinnedSegmentation || c.BoundaryUnit != scalarBoundaryUnit {
		t.Fatalf("corpus header %d %q %q %q does not match the pinned segmentation", c.SchemaVersion, c.UnicodeVersion, c.Segmentation, c.BoundaryUnit)
	}
	if len(c.Vectors) != corpusValidCount || len(c.InvalidEncodings) != corpusInvalidCount {
		t.Fatalf("corpus holds %d vectors and %d invalid encodings, want %d and %d", len(c.Vectors), len(c.InvalidEncodings), corpusValidCount, corpusInvalidCount)
	}
	return c
}

func TestCorpusVectorsKeepScalarsCountsAndBoundaries(t *testing.T) {
	for _, v := range loadInputCorpus(t).Vectors {
		t.Run(v.ID, func(t *testing.T) { assertCorpusVectorMeasures(t, v) })
	}
}

func TestCorpusVectorsFollowTheirPolicy(t *testing.T) {
	for _, v := range loadInputCorpus(t).Vectors {
		t.Run(v.ID, func(t *testing.T) { assertCorpusVectorPolicy(t, v) })
	}
}

func TestCorpusInvalidEncodingsFailWhole(t *testing.T) {
	for _, v := range loadInputCorpus(t).InvalidEncodings {
		t.Run(v.ID, func(t *testing.T) { assertCorpusInvalidEncoding(t, v) })
	}
}

func assertCorpusVectorMeasures(t *testing.T, v inputVector) {
	t.Helper()
	if got, want := []rune(v.Text), corpusScalars(t, v.CodePoints); !slices.Equal(got, want) {
		t.Fatalf("text scalars %U, codePoints %U", got, want)
	}
	if got, err := DecodeUTF8([]byte(v.Text)); err != nil || got != v.Text {
		t.Fatalf("DecodeUTF8 = %q, %v; want the same scalars", got, err)
	}
	counts := [...]struct {
		unit Unit
		want int
	}{{Scalars, v.ScalarCount}, {Bytes, v.UTF8Bytes}, {UTF16Units, v.UTF16Units}}
	for _, c := range counts {
		if got := Count(v.Text, c.unit); got != c.want {
			t.Errorf("Count(unit %d) = %d, want %d", c.unit, got, c.want)
		}
	}
	if got := scalarIndexBoundaries(t, v.Text); !slices.Equal(got, v.GraphemeBoundaries) {
		t.Errorf("grapheme boundaries %v, want %v", got, v.GraphemeBoundaries)
	}
}

type corpusPolicyOutcome struct {
	err  error
	text string
}

func assertCorpusVectorPolicy(t *testing.T, v inputVector) {
	t.Helper()
	typed, typeErr := Draft{}.Replace(corpusField, v.Text)
	checkErr := corpusField.Check(v.Text)
	want := expectedCorpusPolicyOutcome(t, v)
	if !errors.Is(checkErr, want.err) || !errors.Is(typeErr, want.err) || typed.Text != want.text || want.err != nil && typed != (Draft{}) {
		t.Fatalf("%s: Check %v, Replace %q, %v", v.Policy, checkErr, typed.Text, typeErr)
	}
}

func expectedCorpusPolicyOutcome(t *testing.T, v inputVector) corpusPolicyOutcome {
	t.Helper()
	switch v.Policy {
	case "accept":
		return corpusPolicyOutcome{text: v.Text}
	case "multiline_adapter_lf":
		return corpusPolicyOutcome{text: "\n"}
	case "reject_disallowed_control":
		return corpusPolicyOutcome{err: DisallowedControl}
	}
	t.Fatalf("unknown policy %q", v.Policy)
	return corpusPolicyOutcome{}
}

func assertCorpusInvalidEncoding(t *testing.T, v inputInvalid) {
	t.Helper()
	if v.Reason != string(InvalidEncoding) {
		t.Fatalf("reason %q, want %q", v.Reason, InvalidEncoding)
	}
	if v.Encoding == "json_text" {
		assertJSONParseError(t, v)
		return
	}
	if got, err := decodeCorpusInput(t, v); !errors.Is(err, InvalidEncoding) || got != "" {
		t.Fatalf("decode %s = %q, %v; want %v", v.Encoding, got, err, InvalidEncoding)
	}
}

func decodeCorpusInput(t *testing.T, v inputInvalid) (string, error) {
	t.Helper()
	switch v.Encoding {
	case "utf8_hex":
		return DecodeUTF8(corpusBytes(t, v.Input))
	case "utf16_hex":
		return DecodeUTF16(corpusUnits(t, v.Input))
	}
	t.Fatalf("unknown encoding %q", v.Encoding)
	return "", nil
}

func assertJSONParseError(t *testing.T, v inputInvalid) {
	t.Helper()
	if v.RPCErrorCode != jsonRPCParseError {
		t.Fatalf("rpcErrorCode %d, want %d", v.RPCErrorCode, jsonRPCParseError)
	}
	token, err := jsontext.NewDecoder(strings.NewReader(v.Input)).ReadToken()
	var syntactic *jsontext.SyntacticError
	if !errors.As(err, &syntactic) {
		t.Fatalf("strict JSON decoding = %q, %v; want a syntactic error", token.String(), err)
	}
}

func corpusScalars(t *testing.T, codePoints []string) []rune {
	t.Helper()
	scalars := make([]rune, 0, len(codePoints))
	for _, field := range codePoints {
		scalar, err := strconv.ParseUint(field, 16, 32)
		if err != nil || scalar > unicode.MaxRune {
			t.Fatalf("code point %q is not a Unicode scalar: %v", field, err)
			return nil
		}
		scalars = append(scalars, rune(scalar))
	}
	return scalars
}

func corpusBytes(t *testing.T, input string) []byte {
	t.Helper()
	data, err := hex.DecodeString(strings.ReplaceAll(input, " ", ""))
	if err != nil {
		t.Fatalf("hex %q: %v", input, err)
	}
	return data
}

func corpusUnits(t *testing.T, input string) []uint16 {
	t.Helper()
	var units []uint16
	for field := range strings.FieldsSeq(input) {
		unit, err := strconv.ParseUint(field, 16, 16)
		if err != nil {
			t.Fatalf("UTF-16 unit %q: %v", field, err)
		}
		units = append(units, uint16(unit))
	}
	return units
}

func scalarIndexBoundaries(t *testing.T, text string) []int {
	t.Helper()
	var boundaries []int
	for _, offset := range Boundaries(text) {
		scalar, err := Convert(text, offset, Bytes, Scalars)
		if err != nil {
			t.Fatalf("Convert(%d bytes): %v", offset, err)
		}
		boundaries = append(boundaries, scalar)
	}
	return boundaries
}
