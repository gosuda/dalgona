package discovery

import (
	"bufio"
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

func NativeSkills(home, cwd string, extra []string) ([]Skill, error) {
	roots := append([]string{filepath.Join(home, "skills"), filepath.Join(cwd, ".dalgona", "skills")}, extra...)
	out := make([]Skill, 0)
	seen := make(map[string]struct{})
	for _, root := range roots {
		found, err := scanNativeRoot(root, seen)
		if err != nil {
			return nil, err
		}
		out = append(out, found...)
	}
	return out, nil
}

func scanNativeRoot(root string, seen map[string]struct{}) ([]Skill, error) {
	entries, err := nativeRootEntries(root)
	if err != nil {
		return nil, err
	}
	out := make([]Skill, 0)
	for _, entry := range entries {
		skill, ok, err := nativeSkillFromEntry(root, entry)
		if err != nil {
			return nil, err
		}
		if ok && !recordSkill(seen, skill.Name) {
			out = append(out, skill)
		}
	}
	return out, nil
}

func nativeRootEntries(root string) ([]os.DirEntry, error) {
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
	return entries, nil
}

func recordSkill(seen map[string]struct{}, name string) bool {
	if _, exists := seen[name]; exists {
		return true
	}
	seen[name] = struct{}{}
	return false
}

func nativeSkillFromEntry(root string, entry os.DirEntry) (Skill, bool, error) {
	directory, err := nativeSkillDirectory(root, entry)
	if err != nil || !directory {
		return Skill{}, false, err
	}
	path := filepath.Join(root, entry.Name(), "SKILL.md")
	skill, err := parseNative(path)
	if errors.Is(err, os.ErrNotExist) {
		return Skill{}, false, nil
	}
	if err != nil {
		return Skill{}, false, err
	}
	return skill, true, nil
}

func nativeSkillDirectory(root string, entry os.DirEntry) (bool, error) {
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

func parseNative(path string) (Skill, error) {
	body, err := os.ReadFile(path)
	if err != nil {
		return Skill{}, fmt.Errorf("skills: open %s: %w", path, err)
	}
	name, description, err := nativeFrontmatter(path, string(body))
	if err != nil {
		return Skill{}, err
	}
	return Skill{Name: skillName(name, path), Description: description, Path: path}, nil
}

func nativeFrontmatter(path, body string) (string, string, error) {
	lines := strings.Split(strings.ReplaceAll(body, "\r\n", "\n"), "\n")
	if len(lines) == 0 || lines[0] != "---" {
		return "", "", fmt.Errorf("skills: malformed frontmatter: %s", path)
	}
	var name, description string
	for _, line := range lines[1:] {
		if line == "---" {
			return name, description, nil
		}
		key, value, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		switch strings.TrimSpace(key) {
		case "name":
			name = frontmatterValue(value)
		case "description":
			description = frontmatterValue(value)
		}
	}
	return "", "", fmt.Errorf("skills: malformed frontmatter: %s", path)
}

func skillDirs(root string) ([]Skill, error) {
	entries, err := os.ReadDir(root)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read skills directory %s: %w", root, err)
	}
	found := make([]Skill, 0, len(entries))
	for _, entry := range entries {
		directory, err := skillDirectory(root, entry)
		if err != nil {
			return nil, err
		}
		if !directory {
			continue
		}
		skill, err := parseSkill(filepath.Join(root, entry.Name(), "SKILL.md"))
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		found = append(found, skill)
	}
	return found, nil
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
		return false, fmt.Errorf("stat skill directory %s: %w", path, err)
	}
	return info.IsDir(), nil
}

func parseSkill(path string) (Skill, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Skill{}, fmt.Errorf("read skill %s: %w", path, err)
	}
	lines, err := skillFrontmatter(path, data)
	if err != nil {
		return Skill{}, err
	}
	skill := Skill{Path: path}
	for _, line := range lines {
		key, value, ok := strings.Cut(line, ":")
		if !ok {
			continue
		}
		value = frontmatterValue(value)
		switch strings.TrimSpace(key) {
		case "name":
			skill.Name = value
		case "description":
			skill.Description = value
		}
	}
	skill.Name = skillName(skill.Name, path)
	return skill, nil
}

func skillFrontmatter(path string, data []byte) ([]string, error) {
	scanner := bufio.NewScanner(bytes.NewReader(data))
	if !scanner.Scan() || frontmatterLine(scanner.Text()) != "---" {
		if err := scanner.Err(); err != nil {
			return nil, fmt.Errorf("read skill %s: %w", path, err)
		}
		return nil, fmt.Errorf("skills: malformed frontmatter %s", path)
	}
	lines := make([]string, 0)
	for scanner.Scan() {
		line := frontmatterLine(scanner.Text())
		if line == "---" {
			return lines, nil
		}
		lines = append(lines, line)
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("read skill %s: %w", path, err)
	}
	return nil, fmt.Errorf("skills: malformed frontmatter %s", path)
}

func frontmatterLine(line string) string {
	return strings.TrimSuffix(line, "\r")
}

func frontmatterValue(value string) string {
	value = strings.TrimSpace(value)
	if len(value) < 2 {
		return value
	}
	quote := value[0]
	if (quote == '\'' || quote == '"') && value[len(value)-1] == quote {
		return value[1 : len(value)-1]
	}
	return value
}

func skillName(name, path string) string {
	if name == "" {
		return filepath.Base(filepath.Dir(path))
	}
	return name
}
