package rpc

import (
	"encoding/base64"
	"path/filepath"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

type fsEntry struct {
	FileName    string `json:"fileName"`
	IsDirectory bool   `json:"isDirectory"`
	IsFile      bool   `json:"isFile"`
}

func TestFsMethodsRoundTrip(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	dir := t.TempDir()
	file := filepath.Join(dir, "a.txt")
	sub := filepath.Join(dir, "sub")

	c.ok("fs/writeFile", map[string]any{"path": file, "dataBase64": base64.StdEncoding.EncodeToString([]byte("hello"))}, nil)

	var read struct {
		DataBase64 string `json:"dataBase64"`
	}
	c.ok("fs/readFile", map[string]any{"path": file}, &read)
	if got, _ := base64.StdEncoding.DecodeString(read.DataBase64); string(got) != "hello" {
		t.Fatalf("fs/readFile data=%q, want hello", got)
	}

	var meta struct {
		IsDirectory bool `json:"isDirectory"`
		IsFile      bool `json:"isFile"`
		IsSymlink   bool `json:"isSymlink"`
	}
	c.ok("fs/getMetadata", map[string]any{"path": file}, &meta)
	if !meta.IsFile || meta.IsDirectory || meta.IsSymlink {
		t.Fatalf("fs/getMetadata=%+v, want a regular file", meta)
	}

	c.ok("fs/createDirectory", map[string]any{"path": sub}, nil)
	var listing struct {
		Entries []fsEntry `json:"entries"`
	}
	c.ok("fs/readDirectory", map[string]any{"path": dir}, &listing)
	names := make([]string, 0, len(listing.Entries))
	for _, entry := range listing.Entries {
		names = append(names, entry.FileName)
	}
	if want := []string{"a.txt", "sub"}; !slices.Equal(names, want) {
		t.Fatalf("fs/readDirectory names=%v, want %v", names, want)
	}

	c.fails("fs/copy", map[string]any{"sourcePath": sub, "destinationPath": filepath.Join(dir, "copy")}, -32602, "copying a directory needs recursive")
	c.ok("fs/copy", map[string]any{"sourcePath": file, "destinationPath": filepath.Join(sub, "b.txt")}, nil)
	c.ok("fs/remove", map[string]any{"path": file}, nil)
	c.fails("fs/readFile", map[string]any{"path": file}, -32602, "no such file: "+file)
	c.fails("fs/readFile", map[string]any{"path": "relative.txt"}, -32602, "path must be absolute")
}
