package prompt_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/prompt"
)

func TestAssembleFixedOrderAndStable(t *testing.T) {
	t.Parallel()
	in := prompt.Input{
		Base:      "BASE",
		ToolNotes: []string{"READ NOTE", "EXEC NOTE"},
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
	got := prompt.Assemble(in)
	if again := prompt.Assemble(in); got != again {
		t.Fatalf("Assemble changed across identical calls\nfirst: %q\nsecond: %q", got, again)
	}
	assertOrder(t, got, "BASE", "READ NOTE", "EXEC NOTE", "review: Review a change (/skills/review/SKILL.md)", `<instructions path="AGENTS.md">ROOT</instructions>`, "cwd: /workspace", "os: linux", "shell: /bin/sh")
}

func TestAssembleUsesEmbeddedBase(t *testing.T) {
	t.Parallel()
	got := prompt.Assemble(prompt.Input{})
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
