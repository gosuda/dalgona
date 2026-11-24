package document

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
)

var ErrExport = errors.New("export did not complete")

type Extent struct {
	First     Identity
	Last      Identity
	Documents int
	Bytes     int
}

func (s *Scope) Extent(sel Selection) (Extent, error) {
	first, last, err := s.ordered(sel)
	if err != nil {
		return Extent{}, err
	}
	extent := Extent{
		First:     s.entries[first.entry].ref.Header.Identity,
		Last:      s.entries[last.entry].ref.Header.Identity,
		Documents: last.entry - first.entry + 1,
	}
	for i := first.entry; i <= last.entry; i++ {
		from, to := s.sourceRange(i, first, last)
		extent.Bytes += to - from
	}
	return extent, nil
}

func (s *Scope) All() (Selection, error) {
	if len(s.entries) == 0 {
		return Selection{}, ErrNoPosition
	}
	return Selection{Anchor: s.Edge(0, false), Focus: s.Edge(len(s.entries)-1, true)}, nil
}

func (s *Scope) sourceRange(i int, first, last bound) (int, int) {
	from, to := 0, s.entries[i].ref.Length
	if i == first.entry {
		from = min(first.position.Offset, to)
	}
	if i == last.entry {
		to = min(last.position.Offset, to)
	}
	return from, max(from, to)
}

func (s *Scope) WriteMarkdown(ctx context.Context, sel Selection, pager Pager, w io.Writer) error {
	first, last, err := s.ordered(sel)
	if err != nil {
		return err
	}
	tail := byte('\n')
	for i := first.entry; i <= last.entry; i++ {
		from, to := s.sourceRange(i, first, last)
		if i > first.entry {
			if err := writeSeparator(w, tail); err != nil {
				return err
			}
		}
		if from == to {
			continue
		}
		err := s.stream(ctx, i, streamSpan{from: from, to: to}, pager, func(page string) error {
			_, err := io.WriteString(w, page)
			tail = page[len(page)-1]
			return err
		})
		if err != nil {
			return err
		}
	}
	return nil
}

func writeSeparator(w io.Writer, tail byte) error {
	separator := "\n\n"
	if tail == '\n' {
		separator = "\n"
	}
	_, err := io.WriteString(w, separator)
	return err
}

func (d *Document) WriteSource(i int, w io.Writer) error {
	b := d.blocks[i]
	if b.Kind == KindMath && b.Math.Len() > 0 {
		_, err := io.WriteString(w, d.source[b.Math.Start:b.Math.End])
		return err
	}
	switch b.Kind {
	case KindCode, KindFigure, KindMath, KindDiff, KindHTML:
		_, err := io.WriteString(w, b.Projection.Text[b.Body.Start:b.Body.End])
		return err
	case KindParagraph, KindHeading, KindTable, KindRule, KindNotice:
		_, err := io.WriteString(w, d.source[b.Span.Start:b.Span.End])
		return err
	}
	return fmt.Errorf("%w: block kind %d has no source export", ErrExport, b.Kind)
}

type exportDisk interface {
	CreateTemp(dir, pattern string) (exportFile, error)
	Rename(from, to string) error
	Remove(name string) error
	SyncDir(dir string) error
}

type exportFile interface {
	io.Writer
	Name() string
	Sync() error
	Close() error
}

type localDisk struct{}

func (localDisk) CreateTemp(dir, pattern string) (exportFile, error) {
	file, err := os.CreateTemp(dir, pattern)
	if err != nil {
		return nil, err
	}
	return file, nil
}

func (localDisk) Rename(from, to string) error { return os.Rename(from, to) }

func (localDisk) Remove(name string) error { return os.Remove(name) }

func (localDisk) SyncDir(dir string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	directory, err := os.Open(dir) //nolint:gosec // the destination directory comes from the save dialog
	if err != nil {
		return err
	}
	return errors.Join(directory.Sync(), directory.Close())
}

func SaveFile(ctx context.Context, destination string, write func(io.Writer) error) error {
	return saveFile(ctx, localDisk{}, destination, write)
}

func saveFile(ctx context.Context, disk exportDisk, destination string, write func(io.Writer) error) (err error) {
	if destination == "" || !filepath.IsAbs(destination) {
		return fmt.Errorf("%w: choose an absolute destination path, got %q", ErrExport, destination)
	}
	temporary, err := disk.CreateTemp(filepath.Dir(destination), "."+filepath.Base(destination)+".*.partial")
	if err != nil {
		return fmt.Errorf("%w: create a temporary file beside %s: %w", ErrExport, destination, err)
	}
	committed := false
	defer func() {
		if !committed {
			err = errors.Join(err, discard(disk, temporary))
		}
	}()
	buffered := bufio.NewWriterSize(temporary, 64<<10)
	if err := write(buffered); err != nil {
		return fmt.Errorf("%w: %s keeps its previous content: %w", ErrExport, destination, err)
	}
	if err := finishTemporary(ctx, buffered, temporary); err != nil {
		return fmt.Errorf("%w: %s keeps its previous content: %w", ErrExport, destination, err)
	}
	if err := disk.Rename(temporary.Name(), destination); err != nil {
		return fmt.Errorf("%w: replace %s: %w", ErrExport, destination, err)
	}
	committed = true
	if err := disk.SyncDir(filepath.Dir(destination)); err != nil {
		return fmt.Errorf("%w: %s holds the new content, but its folder did not record the replacement durably: %w", ErrExport, destination, err)
	}
	return nil
}

func finishTemporary(ctx context.Context, buffered *bufio.Writer, temporary exportFile) error {
	if err := buffered.Flush(); err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := temporary.Sync(); err != nil {
		return err
	}
	if err := temporary.Close(); err != nil {
		return err
	}
	return ctx.Err()
}

func discard(disk exportDisk, temporary exportFile) error {
	closeErr := temporary.Close()
	if errors.Is(closeErr, os.ErrClosed) {
		closeErr = nil
	}
	if err := disk.Remove(temporary.Name()); err != nil && !errors.Is(err, os.ErrNotExist) {
		return errors.Join(closeErr, err)
	}
	return closeErr
}
