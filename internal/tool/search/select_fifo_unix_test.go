//go:build unix

package search

import (
	"path/filepath"
	"reflect"
	"syscall"
	"testing"
)

func TestSelectDropsNamedPipe(t *testing.T) {
	root := writeSelectTree(t, map[string]string{"a/x.go": "package a\n"})
	if err := syscall.Mkfifo(filepath.Join(root, "a", "pipe.go"), 0o644); err != nil {
		t.Fatal(err)
	}
	got, err := Select(t.Context(), root, "a", "", []string{".go"})
	if err != nil {
		t.Fatal(err)
	}
	if want := []string{"a/x.go"}; !reflect.DeepEqual(got, want) {
		t.Fatalf("Select = %v, want %v", got, want)
	}
}
