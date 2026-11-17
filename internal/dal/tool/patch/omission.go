package patch

import (
	"bytes"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
)

var omissionPhrases = [...]string{
	"rest of", "existing code", "existing implementation", "existing methods",
	"remaining code", "remaining implementation", "remaining methods", "unchanged",
	"previous code", "same as before", "code omitted", "implementation omitted",
	"omitted", "other methods", "keep existing", "keep the rest", "etc",
}

var proseExtensions = [...]string{".md", ".markdown", ".mdx", ".rst", ".txt", ".adoc"}

const ellipsisASCII = "..."

const ellipsisRune = "\u2026"

type observerFinding struct {
	path string
	line int
	text string
}

func blockingFinding(findings []observerFinding) error {
	for _, finding := range findings {
		return refusalError(fmt.Sprintf("patch: %s line %d: %q stands in for omitted code; write the complete text. Nothing was written.", finding.path, finding.line, finding.text))
	}
	return nil
}

func observeOmissions(files []*stagedFile) []observerFinding {
	var findings []observerFinding
	for _, file := range files {
		if file.deleted || slices.Contains(proseExtensions[:], strings.ToLower(filepath.Ext(file.path))) {
			continue
		}
		if line, text, found := omissionLine(file.before, file.after); found {
			findings = append(findings, observerFinding{path: file.path, line: line, text: text})
		}
	}
	return findings
}

func omissionLine(before, after []byte) (int, string, bool) {
	known := make(map[string]int)
	for line := range bytes.SplitSeq(before, []byte("\n")) {
		if text := strings.TrimSpace(string(line)); placeholderLine(text) {
			known[text]++
		}
	}
	for index, line := range bytes.Split(after, []byte("\n")) {
		text := strings.TrimSpace(string(line))
		if !placeholderLine(text) {
			continue
		}
		if known[text] == 0 {
			return index + 1, text, true
		}
		known[text]--
	}
	return 0, "", false
}

func placeholderLine(line string) bool {
	text, ok := commentText(line)
	if !ok {
		return false
	}
	if !strings.Contains(text, ellipsisASCII) && !strings.Contains(text, ellipsisRune) {
		return false
	}
	replacer := strings.NewReplacer(".", " ", ellipsisRune, " ", "(", " ", ")", " ", "[", " ", "]", " ")
	normalized := strings.Join(strings.Fields(strings.ToLower(replacer.Replace(text))), " ")
	if normalized == "" {
		return true
	}
	for _, phrase := range omissionPhrases {
		if normalized == phrase || strings.HasPrefix(normalized, phrase+" ") {
			return true
		}
	}
	return false
}

func commentText(trimmed string) (string, bool) {
	if rest, ok := strings.CutPrefix(trimmed, "<!--"); ok {
		return strings.TrimSpace(strings.TrimSuffix(rest, "-->")), true
	}
	if rest, ok := strings.CutPrefix(trimmed, "/*"); ok {
		return strings.TrimSpace(strings.TrimSuffix(rest, "*/")), true
	}
	if rest, ok := strings.CutPrefix(trimmed, "(*"); ok {
		return strings.TrimSpace(strings.TrimSuffix(rest, "*)")), true
	}
	for _, marker := range []string{"//", "#", "--", ";"} {
		if rest, ok := strings.CutPrefix(trimmed, marker); ok {
			return strings.TrimSpace(rest), true
		}
	}
	return "", false
}

type refusalError string

func (e refusalError) Error() string { return string(e) }
