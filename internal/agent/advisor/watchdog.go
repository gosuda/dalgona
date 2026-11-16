package advisor

import (
	"html"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

const watchdogFile = "WATCHDOG.md"

func Watchdog(home, cwd string) string {
	var out strings.Builder
	for _, path := range watchdogPaths(home, cwd) {
		data, ok := readRegular(path)
		if !ok {
			continue
		}
		if out.Len() > 0 {
			out.WriteString("\n\n")
		}
		out.WriteString(`<watchdog path="`)
		out.WriteString(html.EscapeString(path))
		out.WriteString(`">`)
		out.Write(data)
		out.WriteString("</watchdog>")
	}
	return out.String()
}

func readRegular(path string) ([]byte, bool) {
	info, err := os.Stat(path)
	if err != nil || !info.Mode().IsRegular() {
		return nil, false
	}
	data, err := os.ReadFile(path)
	return data, err == nil
}

func watchdogPaths(home, cwd string) []string {
	paths := []string{filepath.Join(filepath.Clean(home), watchdogFile)}
	for _, dir := range projectDirectories(filepath.Clean(cwd)) {
		for _, path := range [...]string{filepath.Join(dir, watchdogFile), filepath.Join(dir, ".dalgona", watchdogFile)} {
			if !slices.Contains(paths, path) {
				paths = append(paths, path)
			}
		}
	}
	return paths
}

func projectDirectories(cwd string) []string {
	dirs := []string{cwd}
	for current := cwd; ; {
		if isRepositoryRoot(current) {
			slices.Reverse(dirs)
			return dirs
		}
		parent := filepath.Dir(current)
		if parent == current {
			return []string{cwd}
		}
		current = parent
		dirs = append(dirs, current)
	}
}

func isRepositoryRoot(dir string) bool {
	git := filepath.Join(dir, ".git")
	fi, err := os.Stat(git)
	if err != nil {
		return false
	}
	if fi.IsDir() {
		if _, headErr := os.Lstat(filepath.Join(git, "HEAD")); headErr == nil {
			return true
		}
		return false
	}
	if !fi.Mode().IsRegular() {
		return false
	}
	data, err := os.ReadFile(git)
	if err != nil {
		return false
	}
	line, _, _ := strings.Cut(string(data), "\n")
	rest, ok := strings.CutPrefix(strings.TrimSpace(line), "gitdir:")
	if !ok {
		return false
	}
	return strings.TrimSpace(rest) != ""
}
