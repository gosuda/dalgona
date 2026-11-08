package search

import (
	"os"
	"path/filepath"
	"strings"
)

type ignoreRule struct {
	base     string
	matcher  glob
	negate   bool
	dirOnly  bool
	basename bool
}

type ignoreStack struct {
	rules []ignoreRule
}

func loadIgnore(root string) ignoreStack {
	var stack ignoreStack
	stack.loadFile(root, filepath.Join(root, ".git", "info", "exclude"))
	stack.loadFile(root, filepath.Join(root, ".gitignore"))
	return stack
}

func (s *ignoreStack) loadDir(root, dir string) {
	if filepath.Clean(dir) == filepath.Clean(root) {
		return
	}
	s.loadFile(dir, filepath.Join(dir, ".gitignore"))
}

func (s *ignoreStack) loadFile(base, path string) {
	data, err := os.ReadFile(path)
	if err != nil {
		return
	}
	for raw := range strings.SplitSeq(string(data), "\n") {
		line, _ := strings.CutSuffix(raw, "\r")
		rule, ok := parseIgnoreRule(base, line)
		if ok {
			s.rules = append(s.rules, rule)
		}
	}
}

func parseIgnoreRule(base, text string) (ignoreRule, bool) {
	text = strings.TrimRight(text, " \t")
	if text == "" || strings.HasPrefix(text, "#") {
		return ignoreRule{}, false
	}
	rule := ignoreRule{base: base}
	if rest, ok := strings.CutPrefix(text, "!"); ok {
		rule.negate = true
		text = rest
	}
	anchored := strings.HasPrefix(text, "/")
	text, _ = strings.CutPrefix(text, "/")
	text, rule.dirOnly = strings.CutSuffix(text, "/")
	if text == "" {
		return ignoreRule{}, false
	}
	rule.basename = !anchored && !strings.Contains(text, "/")
	matcher, err := compileGlob(text, rule.basename)
	if err != nil {
		return ignoreRule{}, false
	}
	rule.matcher = matcher
	return rule, true
}

func (s ignoreStack) ignored(root, path string, dir bool) bool {
	ignored := false
	for _, rule := range s.rules {
		rel, err := filepath.Rel(rule.base, path)
		if err != nil || rel == "." || outside(rel) {
			continue
		}
		if rule.dirOnly && !dir && !hasIgnoredParent(rule, root, path) {
			continue
		}
		if rule.matcher.match(filepath.ToSlash(rel)) {
			ignored = !rule.negate
		}
	}
	return ignored
}

func hasIgnoredParent(rule ignoreRule, root, path string) bool {
	rel, err := filepath.Rel(rule.base, path)
	if err != nil {
		return false
	}
	parts := strings.Split(filepath.ToSlash(rel), "/")
	for i := 1; i < len(parts); i++ {
		prefix := strings.Join(parts[:i], "/")
		if rule.matcher.match(prefix) {
			return true
		}
	}
	return false
}

func outside(path string) bool {
	return path == ".." || strings.HasPrefix(path, ".."+string(filepath.Separator))
}
