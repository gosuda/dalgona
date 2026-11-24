package textinput

import (
	"encoding/hex"
	json "encoding/json/v2"
	"errors"
	"os"
	"slices"
	"strings"
	"testing"
	"unicode/utf16"
	"unicode/utf8"
)

type corpus struct {
	UnicodeVersion   string           `json:"unicodeVersion"`
	Segmentation     string           `json:"segmentation"`
	BoundaryUnit     string           `json:"boundaryUnit"`
	Vectors          []corpusVector   `json:"vectors"`
	InvalidEncodings []corpusEncoding `json:"invalidEncodings"`
}

type corpusVector struct {
	ID                 string   `json:"id"`
	CodePoints         []string `json:"codePoints"`
	Text               string   `json:"text"`
	ScalarCount        int      `json:"scalarCount"`
	UTF8Bytes          int      `json:"utf8Bytes"`
	UTF16Units         int      `json:"utf16Units"`
	GraphemeBoundaries []int    `json:"graphemeBoundaries"`
	Policy             string   `json:"policy"`
}

type corpusEncoding struct {
	ID       string `json:"id"`
	Encoding string `json:"encoding"`
	Input    string `json:"input"`
	Reason   string `json:"reason"`
}

func loadCorpus(t *testing.T) corpus {
	t.Helper()
	data, err := os.ReadFile("../../../docs/dal/spec/input-text-cases.json")
	if err != nil {
		t.Fatal(err)
	}
	var c corpus
	if err := json.Unmarshal(data, &c); err != nil {
		t.Fatal(err)
	}
	if c.UnicodeVersion != UnicodeVersion || c.BoundaryUnit != "unicode_scalar_index" {
		t.Fatalf("corpus targets Unicode %s in %s, want %s in scalar indices", c.UnicodeVersion, c.BoundaryUnit, UnicodeVersion)
	}
	return c
}

func scalarBoundaries(s string) []int {
	var out []int
	for _, b := range Boundaries(s) {
		out = append(out, utf8.RuneCountInString(s[:b]))
	}
	return out
}

func TestCorpusVectorsKeepTheirCountsAndBoundaries(t *testing.T) {
	for _, v := range loadCorpus(t).Vectors {
		if vectorText(t, v) != v.Text {
			t.Errorf("%s: codePoints and text differ", v.ID)
			continue
		}
		if got := utf8.RuneCountInString(v.Text); got != v.ScalarCount {
			t.Errorf("%s: scalars = %d, want %d", v.ID, got, v.ScalarCount)
		}
		if len(v.Text) != v.UTF8Bytes || utf16Units(v.Text) != v.UTF16Units {
			t.Errorf("%s: bytes, units = %d, %d, want %d, %d", v.ID, len(v.Text), utf16Units(v.Text), v.UTF8Bytes, v.UTF16Units)
		}
		if got := scalarBoundaries(v.Text); !slices.Equal(got, v.GraphemeBoundaries) {
			t.Errorf("%s: boundaries = %v, want %v", v.ID, got, v.GraphemeBoundaries)
		}
	}
}

func vectorText(t *testing.T, v corpusVector) string {
	t.Helper()
	var built strings.Builder
	for _, cp := range v.CodePoints {
		built.WriteRune(parseScalar(t, cp))
	}
	return built.String()
}

func utf16Units(s string) int {
	units := 0
	for _, r := range s {
		units += utf16.RuneLen(r)
	}
	return units
}

func TestCorpusPoliciesHoldForMultilineFields(t *testing.T) {
	for _, v := range loadCorpus(t).Vectors {
		if problem := policyProblem(v); problem != "" {
			t.Errorf("%s: %s", v.ID, problem)
		}
	}
}

func policyProblem(v corpusVector) string {
	err := Field{Name: "prompt"}.Check(v.Text)
	switch v.Policy {
	case "accept":
		return errorText(err)
	case "multiline_adapter_lf":
		return adapterProblem(v.Text, err)
	case "reject_disallowed_control":
		if !isReason(err, DisallowedControl) {
			return "want disallowed_control"
		}
		return ""
	}
	return "unknown policy " + v.Policy
}

func errorText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

func adapterProblem(text string, err error) string {
	converted := NewlinesToLF(text)
	if err != nil || strings.ContainsRune(converted, '\r') || NewlinesToLF(converted) != converted {
		return "the adapter must convert CR once and accept the result"
	}
	return ""
}

func TestCorpusInvalidUTF8FailsAsInvalidEncoding(t *testing.T) {
	checked := 0
	for _, e := range loadCorpus(t).InvalidEncodings {
		if e.Encoding != "utf8_hex" {
			continue
		}
		raw, err := hex.DecodeString(strings.ReplaceAll(e.Input, " ", ""))
		if err != nil {
			t.Fatal(err)
		}
		if err := (Field{Name: "prompt"}).CheckBytes(raw); !isReason(err, Reason(e.Reason)) {
			t.Errorf("%s: err = %v, want %s", e.ID, err, e.Reason)
		}
		checked++
	}
	if checked == 0 {
		t.Fatal("corpus has no UTF-8 encoding vectors")
	}
}

func isReason(err error, reason Reason) bool {
	textErr, ok := errors.AsType[*Error](err)
	return ok && textErr.Reason == reason
}
