package auth

import (
	"bufio"
	"context"
	"errors"
	"io"
	"math"
	"slices"
)

type ProjectionWriters struct {
	Blob, RawTail, Suffix io.Writer
}

func (s *Store) MaskReaderProjection(ctx context.Context, raw *io.SectionReader, suffix []string, output ProjectionWriters) error {
	inputs, err := newProjectionInputs(ctx, raw, suffix)
	if err != nil {
		return err
	}
	plans, err := s.planProjection(inputs)
	if err != nil {
		return err
	}
	if err := inputs.raw.renderBlob(output.Blob, plans.blob); err != nil {
		return err
	}
	return inputs.render(output, plans.view)
}

type projectionInputs struct {
	raw, suffix, joined maskInput
}

var errProjectionExtent = errors.New("auth: projected output extent overflows")

func newProjectionInputs(ctx context.Context, raw *io.SectionReader, suffix []string) (*projectionInputs, error) {
	if raw.Size() == math.MaxInt64 {
		return nil, errProjectionExtent
	}
	suffixStart := raw.Size() + 1
	end := suffixStart
	for _, part := range suffix {
		if int64(len(part)) > math.MaxInt64-end {
			return nil, errProjectionExtent
		}
		end += int64(len(part))
	}
	joined := projectionSource{raw: raw, suffix: suffix}
	return &projectionInputs{
		raw:    maskInput{ctx: ctx, source: raw},
		suffix: maskInput{ctx: ctx, source: io.NewSectionReader(joined, suffixStart, end-suffixStart)},
		joined: maskInput{ctx: ctx, source: io.NewSectionReader(joined, 0, end)},
	}, nil
}

func (p *projectionInputs) render(output ProjectionWriters, plan []replacement) error {
	raw := bufio.NewWriterSize(output.RawTail, maskReadBytes)
	suffix := bufio.NewWriterSize(output.Suffix, maskReadBytes)
	sinks := maskSinks{raw: raw, suffix: suffix, boundary: p.raw.source.Size()}
	if err := p.joined.writeMasked(sinks, plan); err != nil {
		return err
	}
	return errors.Join(raw.Flush(), suffix.Flush())
}

type projectionSource struct {
	raw    *io.SectionReader
	suffix []string
}

func (s projectionSource) ReadAt(dst []byte, offset int64) (int, error) {
	if offset < 0 {
		return 0, errors.New("auth: negative projection offset")
	}
	rawBytes := int(min(int64(len(dst)), max(s.raw.Size()-offset, 0)))
	if rawBytes != 0 {
		n, err := s.raw.ReadAt(dst[:rawBytes], offset)
		if err != nil && !errors.Is(err, io.EOF) {
			return n, err
		}
		if n != rawBytes {
			return n, io.ErrUnexpectedEOF
		}
	}
	if rawBytes == len(dst) {
		return rawBytes, nil
	}
	n := rawBytes + s.copyParts(dst[rawBytes:], offset+int64(rawBytes)-s.raw.Size())
	if n < len(dst) {
		return n, io.EOF
	}
	return n, nil
}

func (s projectionSource) copyParts(dst []byte, offset int64) int {
	read := 0
	if offset == 0 {
		dst[0] = '\n'
		read = 1
	}
	offset = max(0, offset-1)
	for _, part := range s.suffix {
		if read == len(dst) {
			break
		}
		if offset >= int64(len(part)) {
			offset -= int64(len(part))
			continue
		}
		read += copy(dst[read:], part[offset:])
		offset = 0
	}
	return read
}

type projectionPlans struct {
	blob, view []replacement
}

func (s *Store) planProjection(p *projectionInputs) (projectionPlans, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	credentials, err := s.readerCredentials()
	if err != nil {
		return projectionPlans{}, err
	}
	raw, err := s.persistReaderHits(&p.raw, credentials)
	if err != nil {
		return projectionPlans{}, err
	}
	blob := s.readerItems(slices.Clip(credentials), raw)
	hits, err := s.persistProjectionHits(p, credentials)
	if err != nil {
		return projectionPlans{}, err
	}
	return projectionPlans{blob: blob, view: s.readerItems(credentials, append(raw, hits...))}, nil
}

func (s *Store) persistProjectionHits(p *projectionInputs, credentials []replacement) ([]patternHit, error) {
	suffix, err := s.discoverReaderHits(&p.suffix, credentials)
	if err != nil {
		return nil, err
	}
	joined, err := s.discoverReaderHits(&p.joined, credentials)
	if err != nil {
		return nil, err
	}
	if err := context.Cause(p.joined.ctx); err != nil {
		return nil, err
	}
	hits := slices.Concat(suffix, joined)
	if err := s.persistMaskHits(hits); err != nil {
		return nil, err
	}
	return hits, nil
}
