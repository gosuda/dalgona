package patch

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func TestRenamePlanRecordsSourceAbsence(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "before\n"
	writeFixture(t, cwd, "a.txt", source)
	ref := seedReference(env, "a.txt", source, true)
	input := profileInput("a.txt", ref, "MV b.txt")
	plan, err := New("hashline-light").Plan(t.Context(), tool.Call{Args: jsontext.Value(snapshot.JSONString(input))}, env)
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.Files) != 2 {
		t.Fatalf("rename plan files %+v", plan.Files)
	}
	if plan.Files[0].Path != "a.txt" || plan.Files[0].PreSHA != snapshot.Digest([]byte(source)) || plan.Files[0].PostSHA != "" {
		t.Fatalf("source recovery state %+v", plan.Files[0])
	}
	if plan.Files[1].Path != "b.txt" || plan.Files[1].PreSHA != "" || plan.Files[1].PostSHA != snapshot.Digest([]byte(source)) {
		t.Fatalf("destination recovery state %+v", plan.Files[1])
	}
}
