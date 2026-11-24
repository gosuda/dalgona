package inputtext

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"slices"
	"strconv"
	"strings"
	"testing"
	"unicode"
	"unicode/utf8"
)

type breakTest struct {
	path   string
	sha256 string
	cases  int
}

var (
	graphemeBreakTest = breakTest{"testdata/GraphemeBreakTest-18.0.0.txt", "b0cf047ee94485bbdc846de2b902f5f8a815f6b674f9d04223cddadd91c9df31", 853}
	wordBreakTest     = breakTest{"testdata/WordBreakTest-18.0.0.txt", "3dd70c071781276067c680d87303f60434adce7b067bd063194af374edad86a5", 1944}
	sentenceBreakTest = breakTest{"testdata/SentenceBreakTest-18.0.0.txt", "84ca7516e7bd1203ab7e8d888ae77f0a0af0a254d10e09a2847000588848885c", 512}
)

type breakCase struct {
	line       int
	text       string
	boundaries []int
	source     string
}

type segmentation struct {
	boundaries func(string) []int
	next       func(string, int) (int, error)
	previous   func(string, int) (int, error)
}

var (
	words     = segmentation{WordBoundaries, NextWordBoundary, PreviousWordBoundary}
	sentences = segmentation{SentenceBoundaries, NextSentenceBoundary, PreviousSentenceBoundary}
)

func TestBoundariesMatchUnicodeGraphemeBreakTest(t *testing.T) {
	assertBoundaries(t, graphemeBreakTest, Boundaries)
}

func TestWordBoundariesMatchUnicodeWordBreakTest(t *testing.T) {
	assertBoundaries(t, wordBreakTest, WordBoundaries)
}

func TestSentenceBoundariesMatchUnicodeSentenceBreakTest(t *testing.T) {
	assertBoundaries(t, sentenceBreakTest, SentenceBoundaries)
}

func TestSentenceLowercaseLookaheadStopsAtOtherLetters(t *testing.T) {
	if got, want := SentenceBoundaries("x. \u3042b"), []int{0, 3, 7}; !slices.Equal(got, want) {
		t.Errorf("boundaries %v, want %v", got, want)
	}
}

func TestEditingBoundariesMatchFullSegmentation(t *testing.T) {
	var joined strings.Builder
	for _, c := range graphemeBreakTest.load(t) {
		assertEditingBoundaries(t, c.source, c.text, c.boundaries)
		joined.WriteString(c.text)
	}
	text := joined.String()
	assertEditingBoundaries(t, "all cases joined", text, Boundaries(text))
}

func TestWordNavigationMatchesUnicodeWordBreakTest(t *testing.T) {
	assertNavigation(t, wordBreakTest, words)
}

func TestSentenceNavigationMatchesUnicodeSentenceBreakTest(t *testing.T) {
	assertNavigation(t, sentenceBreakTest, sentences)
}

func TestWordAndSentenceNavigationRejectsOffsetsOffScalars(t *testing.T) {
	text := "e\u0301 \U0001F1F0"
	for _, at := range []int{-1, 2, 5, 6, 7, len(text) + 1} {
		for _, navigate := range []func(string, int) (int, error){words.next, words.previous, sentences.next, sentences.previous} {
			if _, err := navigate(text, at); !errors.Is(err, InvalidOffset) {
				t.Errorf("offset %d: error %v, want %v", at, err, InvalidOffset)
			}
		}
	}
}

func assertBoundaries(t *testing.T, test breakTest, boundaries func(string) []int) {
	t.Helper()
	for _, c := range test.load(t) {
		if got := boundaries(c.text); !slices.Equal(got, c.boundaries) {
			t.Errorf("line %d %q: boundaries %v, want %v", c.line, c.source, got, c.boundaries)
		}
	}
}

func assertNavigation(t *testing.T, test breakTest, segments segmentation) {
	t.Helper()
	var joined strings.Builder
	for _, c := range test.load(t) {
		assertNavigationOffsets(t, c.source, c.text, c.boundaries, segments)
		joined.WriteString(c.text)
	}
	text := joined.String()
	assertNavigationOffsets(t, "all cases joined", text, segments.boundaries(text), segments)
}

func assertNavigationOffsets(t *testing.T, name, text string, boundaries []int, segments segmentation) {
	t.Helper()
	for at := 0; at <= len(text); {
		after, _ := slices.BinarySearch(boundaries, at+1)
		before, _ := slices.BinarySearch(boundaries, at)
		wantNext, wantPrevious := boundaries[min(after, len(boundaries)-1)], boundaries[max(before-1, 0)]
		next, nextErr := segments.next(text, at)
		previous, previousErr := segments.previous(text, at)
		if next != wantNext || previous != wantPrevious || nextErr != nil || previousErr != nil {
			t.Errorf("%s: offset %d: next %d, %v and previous %d, %v; want %d and %d", name, at, next, nextErr, previous, previousErr, wantNext, wantPrevious)
		}
		_, size := utf8.DecodeRuneInString(text[at:])
		at += max(size, 1)
	}
}

func assertEditingBoundaries(t *testing.T, name, text string, boundaries []int) {
	t.Helper()
	for offset := range len(text) + 1 {
		want := slices.Contains(boundaries, offset)
		if got := CheckRange(text, Range{Start: offset, End: offset}) == nil; got != want {
			t.Errorf("%s: offset %d accepted %t, want %t", name, offset, got, want)
		}
	}
}

func (test breakTest) load(t *testing.T) []breakCase {
	t.Helper()
	data, err := os.ReadFile(test.path)
	if err != nil {
		t.Fatalf("read %s: %v", test.path, err)
	}
	sum := sha256.Sum256(data)
	if got := hex.EncodeToString(sum[:]); got != test.sha256 {
		t.Fatalf("%s sha256 = %s, want %s", test.path, got, test.sha256)
	}
	cases := parseBreakTest(t, test.path, data)
	if len(cases) != test.cases {
		t.Fatalf("%s: parsed %d cases, want %d", test.path, len(cases), test.cases)
	}
	return cases
}

func parseBreakTest(t *testing.T, path string, data []byte) []breakCase {
	t.Helper()
	var cases []breakCase
	scanner := bufio.NewScanner(bytes.NewReader(data))
	for line := 1; scanner.Scan(); line++ {
		source, _, _ := strings.Cut(scanner.Text(), "#")
		source = strings.TrimSpace(source)
		if source == "" {
			continue
		}
		cases = append(cases, parseBreakLine(t, line, source))
	}
	if err := scanner.Err(); err != nil {
		t.Fatalf("scan %s: %v", path, err)
	}
	return cases
}

func parseBreakLine(t *testing.T, line int, source string) breakCase {
	t.Helper()
	c := breakCase{line: line, source: source}
	var text strings.Builder
	for field := range strings.FieldsSeq(source) {
		switch field {
		case "÷":
			c.boundaries = append(c.boundaries, text.Len())
		case "×":
		default:
			scalar, err := strconv.ParseUint(field, 16, 32)
			if err != nil || scalar > unicode.MaxRune {
				t.Fatalf("line %d: code point %q is not a Unicode scalar: %v", line, field, err)
				return c
			}
			text.WriteRune(rune(scalar))
		}
	}
	c.text = text.String()
	return c
}
