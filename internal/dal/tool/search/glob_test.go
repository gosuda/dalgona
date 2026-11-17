package search

import (
	"path/filepath"
	"testing"
)

func TestRecursiveGlobMatchesRootAndNestedFiles(t *testing.T) {
	matcher, err := compileGlob("**/*.go", false)
	if err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{"a.go", "src/a.go", "src/nested/a.go"} {
		if !matcher.match(path) {
			t.Fatalf("recursive glob did not match %s", path)
		}
	}
	if matcher.match("a.txt") {
		t.Fatal("recursive glob matched wrong extension")
	}
}

func TestRootAnchoredIgnoreRule(t *testing.T) {
	root := t.TempDir()
	stack := ignoreStack{}
	stack.loadFile(root, filepath.Join(root, ".gitignore"))
	rule, ok := parseIgnoreRule(root, "/drop.log")
	if !ok {
		t.Fatal("rule rejected")
	}
	stack.rules = append(stack.rules, rule)
	if !stack.ignored(filepath.Join(root, "drop.log"), false) {
		t.Fatal("root file was not ignored")
	}
	if stack.ignored(filepath.Join(root, "sub", "drop.log"), false) {
		t.Fatal("anchored rule ignored nested file")
	}
}
