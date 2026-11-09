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
	if skill.Name == "" {
		skill.Name = filepath.Base(filepath.Dir(path))
	}
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
