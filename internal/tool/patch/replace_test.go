package patch

import (
	"strings"
	"testing"
)

func TestPlanReplaceRefusesOmissionLine(t *testing.T) {
	env, cwd := editTestEnv(t)
	const source = "package a\n\nfunc f() {\n\treturn\n}\n"
	writeFixture(t, cwd, "a.go", source)
	placeholder := strings.Join([]string{"//", "...", "existing", "code", "..."}, " ")
	after := "package a\n\nfunc f() {\n\t" + placeholder + "\n}\n"
	_, err := PlanReplace(t.Context(), env, "", []ReplacedFile{{
		Path: "a.go", Before: []byte(source), After: []byte(after),
	}})
	want := "patch: a.go line 4: \"" + placeholder + "\" stands in for omitted code; write the complete text. Nothing was written."
	if err == nil || err.Error() != want {
		t.Fatalf("PlanReplace error = %v, want %q", err, want)
	}
	if got := readFixture(t, cwd, "a.go"); got != source {
		t.Fatalf("file changed to %q after a refused plan", got)
	}
}
