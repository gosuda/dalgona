package patch

import (
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

type strictFailMasker struct{}

func (strictFailMasker) Mask(text string) string { return text }

func (strictFailMasker) Unmask(text string) string { return text }

func (strictFailMasker) UnmaskStrict(text string) (string, error) {
	if strings.Contains(text, "{{secret:pattern0:0}}") {
		return "", errors.New("secret placeholder cannot be restored")
	}
	return text, nil
}

func TestPatchStrictFailsWithoutWriting(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "old\n")
	env.Masker = strictFailMasker{}
	input := `{"changes":[{"path":"f.txt","old":"old","new":"{{secret:pattern0:0}}"}]}`
	if _, err := runPatch(t, env, Replace, input); err == nil {
		t.Fatal("patch succeeded, want placeholder restore error")
	} else if !strings.Contains(err.Error(), "cannot be restored") {
		t.Fatalf("patch error = %v, want restore error", err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "old\n" {
		t.Fatalf("file = %q, want unchanged", actual)
	}
	var _ tool.Masker = strictFailMasker{}
}
