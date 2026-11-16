package llmstub

import (
	"cmp"
	"os"
	"regexp"
	"slices"
	"strings"
	"testing"
)

const liveSpecPath = "../../docs/spec/live-debug.md"

var liveTurnReference = regexp.MustCompile(`(?:^|[^A-Za-z0-9_-])(t-[a-z0-9]+(?:-[a-z0-9]+)*)`)

func liveSpecSection(t *testing.T, spec, title string) string {
	t.Helper()
	_, body, found := strings.Cut(spec, "\n## "+title+"\n")
	if !found {
		t.Fatalf("%s has no section %q", liveSpecPath, title)
	}
	section, _, _ := strings.Cut(body, "\n## ")
	return section
}

func liveNamedTurnIDs(tables string) []string {
	var ids []string
	for _, match := range liveTurnReference.FindAllStringSubmatch(tables, -1) {
		if !slices.Contains(ids, match[1]) {
			ids = append(ids, match[1])
		}
	}
	return ids
}

func liveTurnsWithoutRow(script Script, named []string, tables string) []string {
	var orphans []string
	for _, turn := range script.Turns {
		if turn.When.ToolResult != "" || slices.Contains(named, turn.ID) {
			continue
		}
		if !strings.Contains(tables, cmp.Or(turn.When.User, turn.When.AnyText)) {
			orphans = append(orphans, turn.ID)
		}
	}
	return orphans
}

func liveSessionDrift(script Script, tables string) (missing, orphans []string) {
	named := liveNamedTurnIDs(tables)
	for _, id := range named {
		if !slices.ContainsFunc(script.Turns, func(turn Turn) bool { return turn.ID == id }) {
			missing = append(missing, id)
		}
	}
	return missing, liveTurnsWithoutRow(script, named, tables)
}

func TestLiveSessionTurnsExist(t *testing.T) {
	t.Parallel()
	raw, err := os.ReadFile(liveSpecPath)
	if err != nil {
		t.Fatal(err)
	}
	spec := string(raw)
	tables := liveSpecSection(t, spec, "Terminal session") + liveSpecSection(t, spec, "Browser session")
	if len(liveNamedTurnIDs(tables)) == 0 {
		t.Fatalf("the session tables name no scripted turn")
	}
	missing, orphans := liveSessionDrift(loadLiveScript(t), tables)
	for _, id := range missing {
		t.Errorf("turn %s is named in %s but is not in %s", id, liveSpecPath, liveScript)
	}
	for _, id := range orphans {
		t.Errorf("turn %s in %s is driven by no session row in %s", id, liveScript, liveSpecPath)
	}
}

func TestLiveSessionDriftNamesMissingAndUnrowedTurns(t *testing.T) {
	t.Parallel()
	script := mustParse(t, `{`+modelsJSON+`,"turns":[`+
		`{"id":"t-ok","when":{"user":"ok prompt"},"steps":[{"finish":"stop"}]},`+
		`{"id":"t-orphan","when":{"user":"orphan prompt"},"steps":[{"finish":"stop"}]}]}`)
	missing, orphans := liveSessionDrift(script, "|T01|row|`ok prompt` (t-ok), then t-gone|")
	if !slices.Equal(missing, []string{"t-gone"}) || !slices.Equal(orphans, []string{"t-orphan"}) {
		t.Fatalf("missing = %v, orphans = %v, want [t-gone] and [t-orphan]", missing, orphans)
	}
	missing, orphans = liveSessionDrift(script, "|T01|row|`ok prompt` (t-ok), then `orphan prompt`|")
	if len(missing) != 0 || len(orphans) != 0 {
		t.Fatalf("a consistent table gave missing = %v, orphans = %v", missing, orphans)
	}
}
