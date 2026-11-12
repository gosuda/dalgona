package patch

import (
	"bytes"
	"fmt"
	"path/filepath"
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

func checkOmissions(files []*stagedFile) error {
	for _, file := range files {
		if file.deleted || isProseFile(file.path) {
			continue
		}
		if line, text, found := omissionLine(file.before, file.after); found {
			return fmt.Errorf("patch: %s line %d: %q stands in for omitted code; write the complete text. Nothing was written.", file.path, line, text) //nolint:staticcheck // the planned refusal keeps its final period
		}
	}
	return nil
}

func isProseFile(path string) bool {
	ext := strings.ToLower(filepath.Ext(path))
	for _, prose := range proseExtensions {
		if ext == prose {
			return true
		}
	}
	return false
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
	normalized := normalizePlaceholder(text)
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

func normalizePlaceholder(text string) string {
	replacer := strings.NewReplacer(".", " ", ellipsisRune, " ", "(", " ", ")", " ", "[", " ", "]", " ")
	return strings.Join(strings.Fields(strings.ToLower(replacer.Replace(text))), " ")
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
