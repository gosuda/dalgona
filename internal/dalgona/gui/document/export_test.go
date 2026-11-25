package document

import (
	"bytes"
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"
)

func exportScope(t *testing.T) (*Scope, *pages, []string) {
	t.Helper()
	scope := NewScope("local", "thread-1")
	sources := []string{
		"# One\r\n\r\n```go\r\nfunc main() {}\r\n```\r\n",
		"two without newline ![shot](attachment://image-1)",
		"three\n\n$$\nx^2\n$$\n",
	}
	pager := &pages{sources: map[Identity]string{}, size: 3}
	first := markdown(t, "one", sources[0])
	if _, err := scope.Put(first); err != nil {
		t.Fatal(err)
	}
	for i, item := range []string{"two", "three"} {
		id := committed(t, item)
		pager.sources[id] = sources[i+1]
		scope.entries = append(scope.entries, entry{ref: Ref{Header: Header{Identity: id, Format: FormatMarkdown}, Revision: 1, Length: len(sources[i+1])}})
	}
	scope.reindex()
	return scope, pager, sources
}

func TestMarkdownExportReproducesSourceBytes(t *testing.T) {
	scope, pager, sources := exportScope(t)
	all, err := scope.All()
	if err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if writeErr := scope.WriteMarkdown(context.Background(), all, pager, &out); writeErr != nil {
		t.Fatal(writeErr)
	}
	want := sources[0] + "\n" + sources[1] + "\n\n" + sources[2]
	if out.String() != want {
		t.Fatalf("export %q, want %q", out.String(), want)
	}
	extent, extentErr := scope.Extent(all)
	if extentErr != nil || extent.Documents != 3 || extent.Bytes != len(sources[0])+len(sources[1])+len(sources[2]) {
		t.Fatalf("extent %+v (%v)", extent, extentErr)
	}
	checkPartialMarkdownExport(t, scope, pager, sources, out)
}

func checkPartialMarkdownExport(t *testing.T, scope *Scope, pager *pages, sources []string, out bytes.Buffer) {
	t.Helper()
	doc, _ := scope.Loaded(0)
	from := positionAt(t, doc, "func", 0, After)
	partial := Selection{Anchor: from, Focus: Position{Document: scope.Ref(1).Header.Identity, Offset: 3, Affinity: Before}}
	out.Reset()
	if writeErr := scope.WriteMarkdown(context.Background(), partial, pager, &out); writeErr != nil {
		t.Fatal(writeErr)
	}
	at := strings.Index(sources[0], "func")
	if at < 0 {
		t.Fatalf("func is not in %q", sources[0])
	}
	if want := sources[0][at:] + "\n" + sources[1][:3]; out.String() != want {
		t.Fatalf("partial export %q, want %q", out.String(), want)
	}
}

func TestSourceExportKeepsCodeLineEndings(t *testing.T) {
	d := markdown(t, "src", "intro\n\n```go\r\na := 1\r\nb := 2\r\n```\r\n\n```mermaid\ngraph TD\n```\n")
	var code, figure bytes.Buffer
	if err := d.WriteSource(1, &code); err != nil {
		t.Fatal(err)
	}
	if err := d.WriteSource(2, &figure); err != nil {
		t.Fatal(err)
	}
	if code.String() != "a := 1\r\nb := 2\r\n" || figure.String() != "graph TD\n" {
		t.Fatalf("code %q figure %q", code.String(), figure.String())
	}
}

func TestSaveFileReplacesOnlyAfterCompleteContent(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "thread.md")
	if err := os.WriteFile(destination, []byte("previous"), 0o600); err != nil {
		t.Fatal(err)
	}
	scope, pager, _ := exportScope(t)
	all, _ := scope.All()

	pager.fail = errors.New("page 2 unavailable")
	err := SaveFile(context.Background(), destination, func(w io.Writer) error {
		return scope.WriteMarkdown(context.Background(), all, pager, w)
	})
	if !errors.Is(err, ErrExport) || !errors.Is(err, ErrPage) {
		t.Fatalf("failed export returned %v", err)
	}
	assertFile(t, destination, "previous")
	assertOnlyFile(t, dir)

	ctx, cancel := context.WithCancel(context.Background())
	pager.fail = nil
	err = SaveFile(ctx, destination, func(w io.Writer) error {
		cancel()
		return scope.WriteMarkdown(ctx, all, pager, w)
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled export returned %v", err)
	}
	assertFile(t, destination, "previous")
	assertOnlyFile(t, dir)

	var expected bytes.Buffer
	if err := scope.WriteMarkdown(context.Background(), all, pager, &expected); err != nil {
		t.Fatal(err)
	}
	if err := SaveFile(context.Background(), destination, func(w io.Writer) error {
		return scope.WriteMarkdown(context.Background(), all, pager, w)
	}); err != nil {
		t.Fatal(err)
	}
	assertFile(t, destination, expected.String())
	assertOnlyFile(t, dir)

	if err := SaveFile(context.Background(), "relative.md", func(io.Writer) error { return nil }); !errors.Is(err, ErrExport) {
		t.Fatalf("relative destination accepted: %v", err)
	}
}

type recordingDisk struct {
	calls      []string
	failRename error
}

type recordingFile struct {
	disk *recordingDisk
	file exportFile
}

func (d *recordingDisk) CreateTemp(dir, pattern string) (exportFile, error) {
	d.calls = append(d.calls, "create")
	file, err := localDisk{}.CreateTemp(dir, pattern)
	if err != nil {
		return nil, err
	}
	return &recordingFile{disk: d, file: file}, nil
}

func (d *recordingDisk) Rename(from, to string) error {
	d.calls = append(d.calls, "rename")
	if d.failRename != nil {
		return d.failRename
	}
	return localDisk{}.Rename(from, to)
}

func (d *recordingDisk) Remove(name string) error {
	d.calls = append(d.calls, "remove")
	return localDisk{}.Remove(name)
}

func (d *recordingDisk) SyncDir(dir string) error {
	d.calls = append(d.calls, "sync directory")
	return localDisk{}.SyncDir(dir)
}

func (f *recordingFile) Write(p []byte) (int, error) {
	if n := len(f.disk.calls); n == 0 || f.disk.calls[n-1] != "write" {
		f.disk.calls = append(f.disk.calls, "write")
	}
	return f.file.Write(p)
}

func (f *recordingFile) Name() string { return f.file.Name() }

func (f *recordingFile) Sync() error {
	f.disk.calls = append(f.disk.calls, "sync")
	return f.file.Sync()
}

func (f *recordingFile) Close() error {
	f.disk.calls = append(f.disk.calls, "close")
	return f.file.Close()
}

func TestSaveFileFinishesTheTemporaryFileBeforeReplacingAndSyncsTheDirectory(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "thread.md")
	disk := &recordingDisk{}
	if err := saveFile(context.Background(), disk, destination, func(w io.Writer) error {
		_, err := io.WriteString(w, "complete export")
		return err
	}); err != nil {
		t.Fatal(err)
	}
	want := []string{"create", "write", "sync", "close", "rename", "sync directory"}
	if !slices.Equal(disk.calls, want) {
		t.Fatalf("calls %q, want %q", disk.calls, want)
	}
	assertFile(t, destination, "complete export")

	if err := os.WriteFile(destination, []byte("previous"), 0o600); err != nil {
		t.Fatal(err)
	}
	disk = &recordingDisk{failRename: errors.New("destination is busy")}
	err := saveFile(context.Background(), disk, destination, func(w io.Writer) error {
		_, err := io.WriteString(w, "lost export")
		return err
	})
	if !errors.Is(err, ErrExport) {
		t.Fatalf("failed replacement returned %v", err)
	}
	want = []string{"create", "write", "sync", "close", "rename", "close", "remove"}
	if !slices.Equal(disk.calls, want) {
		t.Fatalf("calls after a failed replacement %q, want %q", disk.calls, want)
	}
	assertFile(t, destination, "previous")
	assertOnlyFile(t, dir)
}

func assertFile(t *testing.T, path, want string) {
	t.Helper()
	got, err := os.ReadFile(path)
	if err != nil || string(got) != want {
		t.Fatalf("%s holds %q (%v), want %q", path, got, err, want)
	}
}

func assertOnlyFile(t *testing.T, dir string) {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil || len(entries) != 1 {
		t.Fatalf("directory holds %d entries (%v), want only the destination", len(entries), err)
	}
}

type stallFile struct {
	exportFile
	disk     *recordingDisk
	ctx      context.Context
	released chan struct{}
}

func (f stallFile) Sync() error {
	f.disk.calls = append(f.disk.calls, "sync")
	close(f.released)
	<-f.ctx.Done()
	return nil
}

func (f stallFile) Close() error {
	f.disk.calls = append(f.disk.calls, "close")
	return f.exportFile.Close()
}

type stallDisk struct {
	recordingDisk
	ctx      context.Context
	released chan struct{}
}

func (d *stallDisk) CreateTemp(dir, pattern string) (exportFile, error) {
	d.calls = append(d.calls, "create")
	file, err := localDisk{}.CreateTemp(dir, pattern)
	if err != nil {
		return nil, err
	}
	return stallFile{exportFile: file, disk: &d.recordingDisk, ctx: d.ctx, released: d.released}, nil
}

func TestSaveFileSkipsRenameWhenSyncOutlivesCancellation(t *testing.T) {
	dir := t.TempDir()
	destination := filepath.Join(dir, "thread.md")
	ctx, cancel := context.WithCancel(context.Background())
	disk := &stallDisk{ctx: ctx, released: make(chan struct{})}
	done := make(chan error, 1)
	go func() {
		done <- saveFile(ctx, disk, destination, func(w io.Writer) error {
			_, err := io.WriteString(w, "stalled export")
			return err
		})
	}()
	select {
	case <-disk.released:
	case <-time.After(5 * time.Second):
		t.Fatal("export did not reach Sync")
	}
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("cancelled export returned %v, want the cancellation", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("cancelled export did not return")
	}
	for _, call := range disk.calls {
		if call == "rename" {
			t.Fatalf("calls %q include a rename after cancellation", disk.calls)
		}
	}
	if _, err := os.Stat(destination); !os.IsNotExist(err) {
		t.Fatalf("destination exists after a cancelled export: %v", err)
	}
}
