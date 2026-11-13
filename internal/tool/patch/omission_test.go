package patch

import (
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func omissionDots() string { return strings.Repeat(".", 3) }

func replaceArgs(t *testing.T, path, oldText, newText string) string {
	t.Helper()
	data, err := json.Marshal(map[string]any{"changes": []map[string]string{{"path": path, "old": oldText, "new": newText}}})
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func TestPatchRefusesOmissionPlaceholder(t *testing.T) {
	const source = "package a\n\nfunc f() {\n\treturn\n}\n"
	dots := omissionDots()
	cases := []struct {
		name, oldText, newText string
	}{
		{"go comment", "func f() {\n\treturn\n}", "func f() {\n\t// " + dots + " existing code " + dots + "\n}"},
		{"rest of comment", "return", "// rest of method " + dots},
		{"block comment", "return", "/* " + dots + " */"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			writeFixture(t, cwd, "a.go", source)
			_, err := runPatch(t, env, Replace, replaceArgs(t, "a.go", c.oldText, c.newText))
			if err == nil || !strings.Contains(err.Error(), "stands in for omitted code") {
				t.Fatalf("placeholder edit err = %v, want a stands-in-for-omitted-code refusal", err)
			}
			if got := readFixture(t, cwd, "a.go"); got != source {
				t.Fatalf("refused edit changed the file to %q", got)
			}
		})
	}
	t.Run("apply_patch add", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		input := "*** Begin Patch\n*** Add File: b.py\n+# " + dots + " rest of implementation\n*** End Patch\n"
		_, err := runPatch(t, env, ApplyPatch, input)
		if err == nil || !strings.Contains(err.Error(), "stands in for omitted code") {
			t.Fatalf("add err = %v, want a stands-in-for-omitted-code refusal", err)
		}
		if _, statErr := os.Stat(filepath.Join(cwd, "b.py")); !os.IsNotExist(statErr) {
			t.Fatalf("refused add created b.py: %v", statErr)
		}
	})
}

func TestPatchKeepsPlaceholderAlreadyInFile(t *testing.T) {
	dots := omissionDots()
	source := "package a\n\nfunc f() {\n\t// " + dots + " existing code " + dots + "\n\treturn\n}\n"
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.go", source)
	if _, err := runPatch(t, env, Replace, replaceArgs(t, "a.go", "return", "return 1")); err != nil {
		t.Fatalf("edit elsewhere with a placeholder already in the file: %v", err)
	}
	want := strings.Replace(source, "return", "return 1", 1)
	if got := readFixture(t, cwd, "a.go"); got != want {
		t.Fatalf("file = %q, want %q", got, want)
	}
}

func TestPatchAcceptsOrdinaryEllipsisComments(t *testing.T) {
	dots := omissionDots()
	source := "package a\n\n// wait for the server" + dots + "\n// TODO " + dots + "\n// etcd config " + dots + "\nfunc f() {\n\treturn\n}\n"
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.go", source)
	if _, err := runPatch(t, env, Replace, replaceArgs(t, "a.go", "return", "return 1")); err != nil {
		t.Fatalf("ordinary ellipsis comments: %v", err)
	}
	writeFixture(t, cwd, "notes.md", "# Notes\nold line\n")
	fenced := "```go\n// " + dots + " existing code " + dots + "\n```"
	if _, err := runPatch(t, env, Replace, replaceArgs(t, "notes.md", "old line", fenced)); err != nil {
		t.Fatalf("fenced placeholder in a prose file: %v", err)
	}
}
