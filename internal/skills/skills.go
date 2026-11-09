package skills

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type Skill struct {
	Name        string
	Description string
	Path        string
}

func List(home, cwd string, extra []string) ([]Skill, error) {
	roots := append([]string{filepath.Join(home, "skills"), filepath.Join(cwd, ".dalgona", "skills")}, extra...)
	return listRoots(roots)
}

func listRoots(roots []string) ([]Skill, error) {
	out := make([]Skill, 0)
	seen := make(map[string]struct{})
	for _, root := range roots {
		found, err := scanRoot(root, seen)
		if err != nil {
			return nil, err
		}
		out = append(out, found...)
	}
	return out, nil
}

func scanRoot(root string, seen map[string]struct{}) ([]Skill, error) {
	info, err := os.Stat(root)
	if errors.Is(err, os.ErrNotExist) || (err == nil && !info.IsDir()) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("skills: stat %s: %w", root, err)
	}
	entries, err := os.ReadDir(root)
	if err != nil {
		return nil, fmt.Errorf("skills: read %s: %w", root, err)
	}
	return scanEntries(root, entries, seen)
}

func scanEntries(root string, entries []os.DirEntry, seen map[string]struct{}) ([]Skill, error) {
	out := make([]Skill, 0)
	for _, entry := range entries {
		skill, ok, err := skillFromEntry(root, entry)
		if err != nil {
			return nil, err
		}
		_, exists := seen[skill.Name]
		if !ok || exists {
			continue
		}
		seen[skill.Name] = struct{}{}
		out = append(out, skill)
	}
	return out, nil
}

func skillFromEntry(root string, entry os.DirEntry) (Skill, bool, error) {
	directory, err := skillDirectory(root, entry)
	if err != nil || !directory {
		return Skill{}, false, err
	}
	path := filepath.Join(root, entry.Name(), "SKILL.md")
	skill, err := parse(path)
	if errors.Is(err, os.ErrNotExist) {
		return Skill{}, false, nil
	}
	if err != nil {
		return Skill{}, false, err
	}
	return skill, true, nil
}

func skillDirectory(root string, entry os.DirEntry) (bool, error) {
	if entry.IsDir() {
		return true, nil
	}
	if entry.Type()&os.ModeSymlink == 0 {
		return false, nil
	}
	path := filepath.Join(root, entry.Name())
	info, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("skills: stat %s: %w", path, err)
	}
	return info.IsDir(), nil
}

func parse(path string) (Skill, error) {
	body, err := os.ReadFile(path)
	if err != nil {
		return Skill{}, fmt.Errorf("skills: open %s: %w", path, err)
	}
	name, description, err := frontmatter(path, string(body))
	if err != nil {
		return Skill{}, err
	}
	if name == "" {
		name = filepath.Base(filepath.Dir(path))
	}
	return Skill{Name: name, Description: description, Path: path}, nil
}

func frontmatter(path, body string) (string, string, error) {
	lines := strings.Split(strings.ReplaceAll(body, "\r\n", "\n"), "\n")
	if len(lines) == 0 || lines[0] != "---" {
		return "", "", malformed(path)
	}
	name, description, closed := fields(lines[1:])
	if !closed {
		return "", "", malformed(path)
	}
	return name, description, nil
}

func malformed(path string) error {
	return fmt.Errorf("skills: malformed frontmatter: %s", path)
}

func fields(lines []string) (string, string, bool) {
	var name, description string
	for _, line := range lines {
		if line == "---" {
			return name, description, true
		}
		key, value, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		switch strings.TrimSpace(key) {
		case "name":
			name = unquote(value)
		case "description":
			description = unquote(value)
		}
	}
	return name, description, false
}

func unquote(value string) string {
	value = strings.TrimSpace(value)
	if matchingQuote(value) {
		return value[1 : len(value)-1]
	}
	return value
}

func matchingQuote(value string) bool {
	if len(value) < 2 {
		return false
	}
	quote := value[0]
	if quote != '"' && quote != '\'' {
		return false
	}
	return value[len(value)-1] == quote
}
