package prompt

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/tool"
)

type Mention struct {
	Path string
	Text string
}

func Mentions(prompt, cwd string) []Mention {
	root, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return nil
	}
	mentions := make([]Mention, 0)
	for offset, r := range prompt {
		if r != '@' || !mentionStart(prompt, offset) {
			continue
		}
		path := mentionToken(prompt, offset)
		if path == "" {
			continue
		}
		if mention, ok := loadMention(root, path); ok {
			mentions = append(mentions, mention)
		}
	}
	return mentions
}

func mentionStart(prompt string, offset int) bool {
	if offset == 0 {
		return true
	}
	previous, _ := utf8.DecodeLastRuneInString(prompt[:offset])
	return unicode.IsSpace(previous)
}

func mentionToken(prompt string, offset int) string {
	rest := prompt[offset+1:]
	end := strings.IndexFunc(rest, unicode.IsSpace)
	if end < 0 {
		return rest
	}
	return rest[:end]
}

func loadMention(root, path string) (Mention, bool) {
	resolved, ok := regularWorkspaceFile(root, path)
	if !ok {
		return Mention{}, false
	}
	data, err := os.ReadFile(resolved)
	if err != nil {
		return Mention{}, false
	}
	if bytes.IndexByte(data, 0) >= 0 || !utf8.Valid(data) {
		return Mention{Path: path, Text: "[binary file skipped]"}, true
	}
	text, _ := tool.Project(data, tool.KeepHead, resolved)
	return Mention{Path: path, Text: text}, true
}

func regularWorkspaceFile(root, path string) (string, bool) {
	candidate := path
	if !filepath.IsAbs(candidate) {
		candidate = filepath.Join(root, candidate)
	}
	resolved, err := filepath.EvalSymlinks(candidate)
	if err != nil {
		return "", false
	}
	rel, err := filepath.Rel(root, resolved)
	if err != nil || outsidePath(rel) {
		return "", false
	}
	info, err := os.Stat(resolved)
	if err != nil || !info.Mode().IsRegular() {
		return "", false
	}
	return resolved, true
}
