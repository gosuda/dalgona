package textinput

import (
	"bufio"
	"crypto/sha256"
	"encoding/hex"
	"os"
	"slices"
	"strconv"
	"strings"
	"testing"
)

const graphemeBreakTestSHA256 = "b0cf047ee94485bbdc846de2b902f5f8a815f6b674f9d04223cddadd91c9df31"

type breakCase struct {
	line       int
	text       string
	boundaries []int
}

func graphemeBreakCases(t *testing.T) []breakCase {
	t.Helper()
	data, err := os.ReadFile("testdata/GraphemeBreakTest-18.0.0.txt")
	if err != nil {
		t.Fatal(err)
	}
	if sum := sha256.Sum256(data); hex.EncodeToString(sum[:]) != graphemeBreakTestSHA256 {
		t.Fatalf("GraphemeBreakTest sha256 = %x, want the pinned %s", sum, graphemeBreakTestSHA256)
	}
	var cases []breakCase
	scanner := bufio.NewScanner(strings.NewReader(string(data)))
	for n := 1; scanner.Scan(); n++ {
		line, _, _ := strings.Cut(scanner.Text(), "#")
		if fields := strings.Fields(line); len(fields) > 0 {
			cases = append(cases, parseBreakCase(t, n, fields))
		}
	}
	if len(cases) < 700 {
		t.Fatalf("parsed %d conformance cases, want the full file", len(cases))
	}
	return cases
}

func parseBreakCase(t *testing.T, line int, fields []string) breakCase {
	t.Helper()
	c := breakCase{line: line}
	var text strings.Builder
	for _, field := range fields {
		switch field {
		case "\u00f7":
			c.boundaries = append(c.boundaries, text.Len())
		case "\u00d7":
		default:
			text.WriteRune(parseScalar(t, field))
		}
	}
	c.text = text.String()
	return c
}

func parseScalar(t *testing.T, hexDigits string) rune {
	t.Helper()
	v, err := strconv.ParseUint(hexDigits, 16, 21)
	if err != nil {
		t.Fatal(err)
	}
	return rune(v) //nolint:gosec // ParseUint bounds the value to 21 bits
}

func TestBoundariesPassUnicodeConformance(t *testing.T) {
	for _, c := range graphemeBreakCases(t) {
		if got := Boundaries(c.text); !slices.Equal(got, c.boundaries) {
			t.Errorf("line %d: Boundaries = %v, want %v", c.line, got, c.boundaries)
		}
	}
}

func TestStepsFollowUnicodeConformanceBothWays(t *testing.T) {
	for _, c := range graphemeBreakCases(t) {
		if got := forwardWalk(c.text); !slices.Equal(got, c.boundaries) {
			t.Errorf("line %d: NextBoundary walk = %v, want %v", c.line, got, c.boundaries)
		}
		if got := backwardWalk(c.text); !slices.Equal(got, c.boundaries) {
			t.Errorf("line %d: PrevBoundary walk = %v, want %v", c.line, got, c.boundaries)
		}
		for i := 0; i <= len(c.text); i++ {
			if got, want := IsBoundary(c.text, i), slices.Contains(c.boundaries, i); got != want {
				t.Errorf("line %d: IsBoundary(%d) = %v, want %v", c.line, i, got, want)
			}
		}
	}
}

func forwardWalk(text string) []int {
	walk := []int{0}
	for i := 0; i < len(text); {
		i = NextBoundary(text, i)
		walk = append(walk, i)
	}
	return walk
}

func backwardWalk(text string) []int {
	walk := []int{len(text)}
	for i := len(text); i > 0; {
		i = PrevBoundary(text, i)
		walk = append(walk, i)
	}
	slices.Reverse(walk)
	return walk
}

func TestPrevBoundaryKeepsRegionalIndicatorParityAcrossALongRun(t *testing.T) {
	flag := "\U0001F1F0\U0001F1F7"
	text := strings.Repeat(flag, 40) + "\U0001F1F0"
	end := len(text)
	if got, want := PrevBoundary(text, end), end-len("\U0001F1F0"); got != want {
		t.Fatalf("PrevBoundary = %d, want %d: the trailing lone indicator is its own cluster", got, want)
	}
	if got, want := PrevBoundary(text, end-len("\U0001F1F0")), end-len("\U0001F1F0")-len(flag); got != want {
		t.Fatalf("PrevBoundary = %d, want %d: indicators pair from the start of the run", got, want)
	}
}
