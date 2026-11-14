package prompt

import (
	"cmp"
	"runtime"
	"strings"
)

type Skill struct {
	Name        string
	Description string
	Path        string
}

type Input struct {
	Base         string
	ToolNotes    []string
	Skills       []Skill
	Instructions string
	CWD          string
	OS           string
	Shell        string
}

func Assemble(in Input) string {
	sections := make([]string, 0, 5)
	base := cmp.Or(strings.TrimSpace(in.Base), strings.TrimSpace(System))
	sections = append(sections, base)
	if notes := joinNonEmpty(in.ToolNotes); notes != "" {
		sections = append(sections, "## Tool notes\n\n"+notes)
	}
	if skills := renderSkills(in.Skills); skills != "" {
		sections = append(sections, "## Skills\n\n"+skills)
	}
	if instructions := strings.TrimSpace(in.Instructions); instructions != "" {
		sections = append(sections, "## Project instructions\n\n"+instructions)
	}
	sections = append(sections, renderEnvironment(in))
	return strings.Join(sections, "\n\n") + "\n"
}

func joinNonEmpty(parts []string) string {
	kept := make([]string, 0, len(parts))
	for _, part := range parts {
		if part = strings.TrimSpace(part); part != "" {
			kept = append(kept, part)
		}
	}
	return strings.Join(kept, "\n\n")
}

func renderSkills(skills []Skill) string {
	var out strings.Builder
	for _, skill := range skills {
		if skill.Name == "" {
			continue
		}
		out.WriteString("- ")
		out.WriteString(skill.Name)
		if skill.Description != "" {
			out.WriteString(": ")
			out.WriteString(skill.Description)
		}
		if skill.Path != "" {
			out.WriteString(" (")
			out.WriteString(skill.Path)
			out.WriteByte(')')
		}
		out.WriteByte('\n')
	}
	return strings.TrimSuffix(out.String(), "\n")
}

func renderEnvironment(in Input) string {
	operatingSystem := cmp.Or(in.OS, runtime.GOOS)
	var out strings.Builder
	out.WriteString("## Environment\n\ncwd: ")
	out.WriteString(in.CWD)
	out.WriteString("\nos: ")
	out.WriteString(operatingSystem)
	out.WriteString("\nshell: ")
	out.WriteString(in.Shell)
	return out.String()
}
