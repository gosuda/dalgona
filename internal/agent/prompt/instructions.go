package prompt

import (
	"fmt"
	"html"
	"io"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

const (
	instructionLimit       = 32 * 1024
	nestedInstructionLimit = 8 * 1024
)

func Instructions(home, cwd string) string {
	home = filepath.Clean(home)
	homeAgents := filepath.Join(home, "AGENTS.md")
	paths := []string{homeAgents}
	root := repositoryRoot(cwd)
	for _, dir := range directoriesDown(root, cwd) {
		path := firstInstruction(dir)
		if filepath.Clean(path) != homeAgents {
			paths = append(paths, path)
		}
	}
	return renderInstructionFiles(paths, instructionLimit)
}

func Nested(cwd, readPath string, seen map[string]bool) (string, []string) {
	root, resolved, ok := resolvedReadPath(cwd, readPath)
	if !ok {
		return "", nil
	}
	rel, err := filepath.Rel(root, filepath.Dir(resolved))
	if err != nil || rel == "." || outsidePath(rel) {
		return "", nil
	}
	return renderNewInstructions(nestedInstructionPaths(root, rel), seen)
}

func repositoryRoot(cwd string) string {
	current := filepath.Clean(cwd)
	for {
		if _, err := os.Lstat(filepath.Join(current, ".git")); err == nil {
			return current
		}
		parent := filepath.Dir(current)
		if parent == current {
			return filepath.Clean(cwd)
		}
		current = parent
	}
}

func directoriesDown(root, cwd string) []string {
	rel, err := filepath.Rel(root, cwd)
	if err != nil || outsidePath(rel) {
		return []string{filepath.Clean(cwd)}
	}
	if rel == "." {
		return []string{filepath.Clean(root)}
	}
	dirs := make([]string, 0, strings.Count(rel, string(filepath.Separator))+2)
	current := filepath.Clean(root)
	dirs = append(dirs, current)
	for part := range strings.SplitSeq(rel, string(filepath.Separator)) {
		current = filepath.Join(current, part)
		dirs = append(dirs, current)
	}
	return dirs
}

func firstInstruction(dir string) string {
	agents := filepath.Join(dir, "AGENTS.md")
	if info, err := os.Stat(agents); err == nil && info.Mode().IsRegular() {
		return agents
	}
	return filepath.Join(dir, "CLAUDE.md")
}

func renderInstructionFiles(paths []string, limit int) string {
	sections := make([]string, 0, len(paths))
	for _, path := range paths {
		if path == "" {
			continue
		}
		if text, ok := readCapped(path, limit); ok {
			sections = append(sections, wrapInstruction(path, text))
		}
	}
	return strings.Join(sections, "\n\n")
}

func renderNewInstructions(paths []string, seen map[string]bool) (string, []string) {
	sections := make([]string, 0, len(paths))
	newlySeen := make([]string, 0, len(paths))
	for _, path := range paths {
		if seen[path] {
			continue
		}
		if text, ok := readCapped(path, nestedInstructionLimit); ok {
			seen[path] = true
			newlySeen = append(newlySeen, path)
			sections = append(sections, wrapInstruction(path, text))
		}
	}
	return strings.Join(sections, "\n\n"), newlySeen
}

func nestedInstructionPaths(root, rel string) []string {
	parts := strings.Split(rel, string(filepath.Separator))
	paths := make([]string, 0, len(parts)*2)
	current := root
	for _, part := range parts {
		current = filepath.Join(current, part)
		paths = append(paths, filepath.Join(current, "AGENTS.md"), filepath.Join(current, "CLAUDE.md"))
	}
	return paths
}

func resolvedReadPath(cwd, readPath string) (string, string, bool) {
	root, err := filepath.EvalSymlinks(cwd)
	if err != nil {
		return "", "", false
	}
	candidate := readPath
	if !filepath.IsAbs(candidate) {
		candidate = filepath.Join(root, candidate)
	}
	resolved, err := filepath.EvalSymlinks(candidate)
	if err != nil {
		return "", "", false
	}
	return root, resolved, true
}

func outsidePath(rel string) bool {
	return rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) || filepath.IsAbs(rel)
}

func readCapped(path string, limit int) (string, bool) {
	info, err := os.Stat(path)
	if err != nil || !info.Mode().IsRegular() {
		return "", false
	}
	file, err := os.Open(path)
	if err != nil {
		return "", false
	}
	data, readErr := io.ReadAll(io.LimitReader(file, int64(limit)+1))
	closeErr := file.Close()
	if readErr != nil || closeErr != nil {
		return "", false
	}
	if len(data) <= limit {
		return string(data), true
	}
	return truncateInstruction(data, limit), true
}

func truncateInstruction(data []byte, limit int) string {
	end := limit
	for end > 0 && !utf8.RuneStart(data[end]) {
		end--
	}
	text := string(data[:end])
	if !strings.HasSuffix(text, "\n") {
		text += "\n"
	}
	return fmt.Sprintf("%s[instructions truncated at %d KiB]\n", text, limit/1024)
}

func wrapInstruction(path, text string) string {
	return `<instructions path="` + html.EscapeString(path) + `">` + text + `</instructions>`
}
