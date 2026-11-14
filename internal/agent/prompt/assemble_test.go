package prompt_test

import (
	"runtime"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/prompt"
)

func TestAssembleFixedOrderAndStable(t *testing.T) {
	t.Parallel()
	in := prompt.Input{
		Base:      "BASE",
		ToolNotes: []string{"READ NOTE", "  EXEC NOTE  "},
		Skills: []prompt.Skill{{
			Name:        "review",
			Description: "Review a change",
			Path:        "/skills/review/SKILL.md",
		}},
		Instructions: `<instructions path="AGENTS.md">ROOT</instructions>`,
		CWD:          "/workspace",
		OS:           "linux",
		Shell:        "/bin/sh",
	}
	notes := slices.Clone(in.ToolNotes)
	got := prompt.Assemble(in)
	if again := prompt.Assemble(in); got != again {
		t.Fatalf("Assemble changed across identical calls\nfirst: %q\nsecond: %q", got, again)
	}
	assertOrder(t, got, "BASE", "READ NOTE", "EXEC NOTE", "review: Review a change (/skills/review/SKILL.md)", `<instructions path="AGENTS.md">ROOT</instructions>`, "cwd: /workspace", "os: linux", "shell: /bin/sh")
	if !slices.Equal(in.ToolNotes, notes) {
		t.Errorf("Assemble mutated ToolNotes\nbefore: %q\nafter:  %q", notes, in.ToolNotes)
	}
}

func TestAssembleUsesEmbeddedBase(t *testing.T) {
	t.Parallel()
	got := prompt.Assemble(prompt.Input{})
	if blank := prompt.Assemble(prompt.Input{Base: "   "}); blank != got {
		t.Errorf("whitespace-only Base changed the default prompt\nblank:   %q\ndefault: %q", blank, got)
	}
	custom := prompt.Assemble(prompt.Input{Base: "  CUSTOM  "})
	if !strings.HasPrefix(custom, "CUSTOM") {
		t.Errorf("non-empty Base was not used as the prompt base, got prefix %q", custom[:min(len(custom), 32)])
	}
	for _, want := range []string{"Use `read`", "Use `patch` or `apply_patch`", "A declined call returns its reason"} {
		if !strings.Contains(got, want) {
			t.Errorf("default prompt does not contain %q", want)
		}
	}
}

func assertOrder(t *testing.T, text string, parts ...string) {
	t.Helper()
	at := 0
	for _, part := range parts {
		index := strings.Index(text[at:], part)
		if index < 0 {
			t.Fatalf("%q not found after byte %d in:\n%s", part, at, text)
		}
		at += index + len(part)
	}
}

func TestAssembleOmitsEmptySections(t *testing.T) {
	t.Parallel()
	got := prompt.Assemble(prompt.Input{
		Base:         "BASE",
		ToolNotes:    []string{"", "   "},
		Instructions: "  \n\t ",
		Skills: []prompt.Skill{
			{Name: "review", Description: "Review a change", Path: "/skills/review/SKILL.md"},
			{Name: "", Description: "nameless skill", Path: "/skills/nameless/SKILL.md"},
		},
	})
	for _, absent := range []string{"## Tool notes", "## Project instructions"} {
		if strings.Contains(got, absent) {
			t.Errorf("empty input produced heading %q in:\n%s", absent, got)
		}
	}
	if strings.Contains(got, "nameless skill") || strings.Contains(got, "/skills/nameless/") {
		t.Errorf("nameless skill was rendered in:\n%s", got)
	}
	if !strings.Contains(got, "os: "+runtime.GOOS) {
		t.Errorf("empty OS did not default to runtime.GOOS in:\n%s", got)
	}
}
