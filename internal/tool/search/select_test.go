package search

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"testing"
)

func writeSelectTree(t *testing.T, files map[string]string) string {
	t.Helper()
	root := t.TempDir()
	for rel, body := range files {
		path := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	return root
}

func TestSelectByExtensionAndPath(t *testing.T) {
	root := writeSelectTree(t, map[string]string{
		"a/x.go": "package a\n",
		"a/y.ts": "export {};\n",
		"b/z.go": "package b\n",
	})
	got, err := Select(t.Context(), root, "a", "", []string{".go"})
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"a/x.go"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("Select = %v, want %v", got, want)
	}
}

func TestSelectGlobMatchesBaseNameAtAnyDepth(t *testing.T) {
	root := writeSelectTree(t, map[string]string{
		"a/x.go": "package a\n",
		"a/y.ts": "export {};\n",
		"b/z.go": "package b\n",
	})
	got, err := Select(t.Context(), root, ".", "z.go", []string{".go"})
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"b/z.go"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("Select = %v, want %v", got, want)
	}
}

func TestSelectRefusesPathOutsideWorkspace(t *testing.T) {
	root := writeSelectTree(t, map[string]string{"a/x.go": "package a\n"})
	_, err := Select(t.Context(), root, "../x", "", []string{".go"})
	if err == nil || err.Error() != "search: path must stay within the workspace" {
		t.Fatalf("Select error = %v, want path refusal", err)
	}
}

func TestSelectSkipsSymbolicLinks(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("creating a symbolic link needs a privilege the test does not have on Windows")
	}
	root := writeSelectTree(t, map[string]string{"a/x.go": "package a\n"})
	if err := os.Symlink(filepath.Join(root, "a", "x.go"), filepath.Join(root, "link.go")); err != nil {
		t.Fatal(err)
	}
	got, err := Select(t.Context(), root, ".", "", []string{".go"})
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"a/x.go"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("Select = %v, want %v", got, want)
	}
}

func TestSelectDropsBinaryFile(t *testing.T) {
	root := writeSelectTree(t, map[string]string{
		"a/x.go":   "package a\n",
		"a/bin.go": "package a\x00\n",
	})
	got, err := Select(t.Context(), root, "a", "", []string{".go"})
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"a/x.go"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("Select = %v, want %v", got, want)
	}
}

func TestSelectReturnsContextError(t *testing.T) {
	rows := []struct {
		name  string
		files map[string]string
	}{
		{name: "populated directory", files: map[string]string{"a/x.go": "package a\n"}},
		{name: "empty directory", files: map[string]string{}},
	}
	for _, row := range rows {
		t.Run(row.name, func(t *testing.T) {
			root := writeSelectTree(t, row.files)
			ctx, cancel := context.WithCancel(t.Context())
			cancel()
			_, err := Select(ctx, root, ".", "", []string{".go"})
			if !errors.Is(err, context.Canceled) {
				t.Fatalf("Select error = %v, want context.Canceled", err)
			}
		})
	}
}
