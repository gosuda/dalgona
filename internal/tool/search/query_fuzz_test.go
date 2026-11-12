package search

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
)

func matchesAnyLine(re *regexp.Regexp, text string) bool {
	for line := range strings.SplitSeq(text, "\n") {
		if re.MatchString(strings.TrimSuffix(line, "\r")) {
			return true
		}
	}
	return false
}

func FuzzTrigramCandidates(f *testing.F) {
	f.Add("needle", "a needle\n")
	f.Add("(?i)k", "\u212a\n")
	f.Add("[a-c]+x", "abx\n")
	f.Fuzz(func(t *testing.T, pattern, text string) {
		if !utf8.ValidString(text) {
			t.Skip("the engine reads only valid UTF-8")
		}
		re, err := regexp.Compile(pattern)
		if err != nil {
			t.Skip("the pattern does not compile")
		}
		requireChangeTimes(t)
		ws := t.TempDir()
		writeBuildFiles(t, ws, map[string]string{"f.txt": text})
		start := time.Now().Add(time.Hour)
		dir := buildTestIndex(t, ws, start, 1<<30)
		filter, err := openTrigramFilter(dir, pattern, clockAt(start))
		if err != nil {
			t.Fatal(err)
		}
		info, err := os.Stat(filepath.Join(ws, "f.txt"))
		if err != nil {
			t.Fatal(err)
		}
		read, _ := filter.decide("f.txt", info)
		if matchesAnyLine(re, text) && !read {
			t.Fatalf("pattern %q skips f.txt, which has a matching line in %q", pattern, text)
		}
	})
}
