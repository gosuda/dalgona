package agent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type runOutput struct {
	id       string
	path     string
	observed int64
}

func (w *worker) execOutputPages(ctx context.Context, run runOutput) (spillPages, error) {
	info, err := w.t.runner.Stat(ctx, run.path)
	if run.observed == 0 && errors.Is(err, fs.ErrNotExist) {
		return spillPages{}, nil
	}
	if err != nil {
		return spillPages{}, fmt.Errorf("agent: stat exec output %s: %w", run.path, err)
	}
	if info.Size < run.observed {
		return spillPages{}, fmt.Errorf("agent: incomplete exec output %s: recorded %d of %d observed bytes: %w", run.path, info.Size, run.observed, io.ErrUnexpectedEOF)
	}
	return spillPages{size: info.Size, read: func(ctx context.Context, offset, limit int64) ([]byte, int64, error) {
		page, err := runner.ReadProcessPage(ctx, w.t.runner, run.id, offset, limit)
		return page.Data, page.Next, err
	}}, nil
}

func (s spillPages) inline(ctx context.Context, path string) ([]byte, error) {
	if s.size == 0 {
		return nil, nil
	}
	data, next, err := s.read(ctx, 0, s.size)
	if err != nil {
		return nil, fmt.Errorf("agent: read exec output %s: %w", path, err)
	}
	if int64(len(data)) != s.size || next != s.size {
		return nil, fmt.Errorf("agent: incomplete exec output %s: %w", path, io.ErrUnexpectedEOF)
	}
	return data, nil
}

func (w *worker) publishRunSpill(ctx context.Context, source spillPages, path string, runErr error) (string, *event.Spill, error) {
	notice := fmt.Sprintf("[output truncated: %d bytes total; full output: %s; page it with read offset/limit]", source.size, path)
	parts := []string{notice}
	if runErr != nil {
		parts = append(parts, "\n", runErr.Error())
	}
	var projection string
	sha, size, err := w.t.host.journal.PutBlobStream(ctx, nil, func(dst io.Writer) error {
		raw := spillTail{limit: event.MaxInlineBytes}
		suffix := spillTail{limit: event.MaxProjectionBytes}
		sinks := auth.ProjectionWriters{Blob: dst, RawTail: &raw, Suffix: &suffix}
		if err := writeSpillProjection(ctx, w.t.host.cfg.Auth, source, parts, sinks); err != nil {
			return err
		}
		projection = string(raw.bytes(event.MaxProjectionBytes-len(suffix.tail))) + string(suffix.bytes(event.MaxProjectionBytes))
		return nil
	})
	if err != nil {
		return "", nil, fmt.Errorf("agent: spill exec output %s: %w", path, err)
	}
	spill := &event.Spill{Path: "blobs/" + sha, Bytes: size, SHA256: sha}
	return projection, spill, nil
}

func writeSpillProjection(ctx context.Context, store *auth.Store, source spillPages, parts []string, out auth.ProjectionWriters) error {
	if store == nil {
		return copySpillProjection(ctx, source, parts, out)
	}
	return source.withCapture(ctx, func(input *io.SectionReader) error {
		if err := store.MaskReaderProjection(ctx, input, parts, out); err != nil {
			return fmt.Errorf("agent: secret masking failed: %w", err)
		}
		return nil
	})
}

func copySpillProjection(ctx context.Context, source spillPages, parts []string, out auth.ProjectionWriters) error {
	if err := source.copy(ctx, io.MultiWriter(out.Blob, out.RawTail)); err != nil {
		return err
	}
	if _, err := io.WriteString(out.Suffix, "\n"); err != nil {
		return err
	}
	for _, part := range parts {
		if _, err := io.WriteString(out.Suffix, part); err != nil {
			return err
		}
	}
	return nil
}

func writeSpill(ctx context.Context, store *auth.Store, source spillPages, dst io.Writer) error {
	if store == nil {
		return source.copy(ctx, dst)
	}
	return source.withCapture(ctx, func(input *io.SectionReader) error {
		if err := store.MaskReader(ctx, input, dst); err != nil {
			return fmt.Errorf("agent: secret masking failed: %w", err)
		}
		return nil
	})
}

func (s spillPages) withCapture(ctx context.Context, consume func(*io.SectionReader) error) (retErr error) {
	snapshot, err := newSpillSnapshot(ctx, s.size)
	if err != nil {
		return err
	}
	defer func() { retErr = errors.Join(retErr, snapshot.close()) }()
	if err := s.each(ctx, snapshot.append); err != nil {
		return err
	}
	return consume(io.NewSectionReader(snapshot, 0, s.size))
}

func (s spillPages) copy(ctx context.Context, dst io.Writer) error {
	return s.each(ctx, func(_ int64, data []byte) error {
		_, err := dst.Write(data)
		return err
	})
}

type spillPages struct {
	size int64
	read func(context.Context, int64, int64) ([]byte, int64, error)
}

func (s spillPages) each(ctx context.Context, emit func(int64, []byte) error) error {
	for offset := int64(0); offset < s.size; {
		if err := context.Cause(ctx); err != nil {
			return err
		}
		limit := min(runner.MaxProcessPageBytes, s.size-offset)
		data, next, err := s.read(ctx, offset, limit)
		if err != nil {
			return err
		}
		if int64(len(data)) != limit || next-offset != limit {
			return fmt.Errorf("agent: incomplete spill page at %d: %w", offset, io.ErrUnexpectedEOF)
		}
		if err := emit(offset, data); err != nil {
			return err
		}
		offset = next
	}
	return nil
}

type spillTail struct {
	tail    []byte
	limit   int
	clipped bool
}

func (s *spillTail) Write(data []byte) (int, error) {
	keep := s.room(len(data))
	s.tail = append(s.tail, data[len(data)-keep:]...)
	return len(data), nil
}

func (s *spillTail) WriteString(data string) (int, error) {
	keep := s.room(len(data))
	s.tail = append(s.tail, data[len(data)-keep:]...)
	return len(data), nil
}

func (s *spillTail) room(size int) int {
	if s.tail == nil {
		s.tail = make([]byte, 0, s.limit)
	}
	n := min(size, s.limit)
	if size > s.limit-len(s.tail) {
		s.clipped = true
		start := len(s.tail) + n - s.limit
		s.tail = s.tail[:copy(s.tail, s.tail[start:])]
	}
	return n
}

func (s *spillTail) bytes(limit int) []byte {
	tail := s.tail[max(len(s.tail)-limit, 0):]
	if s.clipped || len(tail) != len(s.tail) {
		for len(tail) > 0 && !utf8.RuneStart(tail[0]) {
			tail = tail[1:]
		}
	}
	return tail
}
