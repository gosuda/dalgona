package patch

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"
)

func TestRawIdentityNoops(t *testing.T) {
	tests := []struct {
		name string
		data []byte
		body string
	}{
		{"lf", []byte("one\ntwo\n"), "PUT 1:\n+one\n"},
		{"crlf", []byte("one\r\ntwo\r\n"), "PUT 1:\n+one\n"},
		{"bom_crlf", append([]byte{0xef, 0xbb, 0xbf}, []byte("one\r\ntwo\r\n")...), "PUT 1:\n+one\n"},
		{"unterminated", []byte("one"), "PUT 1:\n+one"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			path := filepath.Join(cwd, "f.txt")
			if err := os.WriteFile(path, test.data, 0o644); err != nil {
				t.Fatal(err)
			}
			ref := env.Snapshots.Put("f.txt", test.data, nil)
			result, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, test.body))
			if err != nil || string(result.Output) != "no_change\n" {
				t.Fatalf("identity: %+v %v", result, err)
			}
			actual, err := os.ReadFile(path)
			if err != nil || !bytes.Equal(actual, test.data) {
				t.Fatalf("identity changed bytes: %v %v", actual, err)
			}
		})
	}
}

func TestOpaqueTextEditEndsLineage(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, Replace, `{"changes":[{"path":"f.txt","old":"one","new":"ONE"}]}`); err != nil {
		t.Fatal(err)
	}
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT 2:\n+TWO\n")); err == nil {
		t.Fatal("opaque text edit retained lineage")
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "ONE\ntwo\n" {
		t.Fatalf("unexpected bytes %q", actual)
	}
}

func TestDeleteFinalUnterminatedLine(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "a\nlast"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "CUT 2")); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "a\n" {
		t.Fatalf("delete result %q", actual)
	}
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT 2:\n+other\n")); err == nil {
		t.Fatal("deleted final line remained mapped")
	}
}

func TestRemoveAfterABAChangeConflicts(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\n"
	writeFixture(t, cwd, "f.txt", source)
	oldRef := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", oldRef, "PUT 1:\n+changed\n")); err != nil {
		t.Fatal(err)
	}
	newRef := seedReference(env, "f.txt", readFixture(t, cwd, "f.txt"), true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", newRef, "PUT 1:\n+one\n")); err != nil {
		t.Fatal(err)
	}
	if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", oldRef, "REM")); err == nil {
		t.Fatal("REM accepted ABA source")
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != source {
		t.Fatalf("REM conflict changed file %q", actual)
	}
}
