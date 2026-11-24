package auth

import (
	"bufio"
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

const maskReadBytes = 32 << 10

func (s *Store) MaskReader(ctx context.Context, input *io.SectionReader, output io.Writer) error {
	reader := maskInput{ctx: ctx, source: input}
	items, err := s.readerReplacements(&reader)
	if err != nil {
		return err
	}
	return reader.renderBlob(output, items)
}

func (s *Store) readerReplacements(input *maskInput) ([]replacement, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	credentials, err := s.readerCredentials()
	if err != nil {
		return nil, err
	}
	hits, err := s.persistReaderHits(input, credentials)
	if err != nil {
		return nil, err
	}
	return s.readerItems(credentials, hits), nil
}

func (s *Store) readerCredentials() ([]replacement, error) {
	if err := s.reloadAuthIfChanged(); err != nil {
		s.maskErr = fmt.Errorf("reload credentials: %w", err)
		return nil, s.maskErr
	}
	if !s.secretsDirty {
		s.maskErr = nil
	}
	return s.credentialReplacements(), nil
}

func (s *Store) persistReaderHits(input *maskInput, credentials []replacement) ([]patternHit, error) {
	hits, err := s.discoverReaderHits(input, credentials)
	if err != nil {
		return nil, err
	}
	if err := context.Cause(input.ctx); err != nil {
		return nil, err
	}
	if err := s.persistMaskHits(hits); err != nil {
		return nil, err
	}
	return hits, nil
}

func (s *Store) discoverReaderHits(input *maskInput, credentials []replacement) ([]patternHit, error) {
	collector := readerHits{input: input, known: s.secrets, seen: make(map[hitSize][]string)}
	if err := collector.patterns(s.patterns); err != nil {
		return nil, err
	}
	if s.builtin {
		if err := collector.builtins(credentials); err != nil {
			return nil, err
		}
	}
	return collector.hits, nil
}

func (s *Store) readerItems(credentials []replacement, hits []patternHit) []replacement {
	items := credentials
	for _, hit := range hits {
		if hit.family != familyPattern {
			continue
		}
		if placeholder, ok := findPatternPlaceholder(s.secrets, patternPrefix(hit.family, hit.index), hit.value); ok {
			items = append(items, replacement{from: hit.value, to: placeholder})
		}
	}
	return append(items, s.recordedBuiltinReplacements(nil)...)
}

type hitSize struct {
	family string
	index  int
	size   int64
}

type readerHits struct {
	input *maskInput
	known map[string]string
	seen  map[hitSize][]string
	hits  []patternHit
}

func (h *readerHits) patterns(patterns []pattern) error {
	for _, configured := range patterns {
		matcher, err := secrets.NewRegexpReader(h.input.ctx, h.input.source, configured.re.String())
		if err != nil {
			return err
		}
		if err := h.pattern(matcher, configured.index); err != nil {
			return err
		}
	}
	return nil
}

func (h *readerHits) pattern(matcher *secrets.RegexpReader, index int) error {
	for {
		start, end, err := matcher.Next()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		if end-start < 8 {
			continue
		}
		if err := h.remember(hitSize{family: familyPattern, index: index, size: end - start}, start); err != nil {
			return err
		}
	}
}

func (h *readerHits) builtins(credentials []replacement) error {
	var placeholders placeholderReader
	return secrets.ScanReader(h.input.ctx, h.input.source, func(hit secrets.ReaderHit) error {
		inside, err := placeholders.covers(h.input, hit)
		if inside || err != nil {
			return err
		}
		covered, err := h.input.covered(hit, credentials)
		if covered || err != nil {
			return err
		}
		return h.remember(hitSize{family: familyBuiltin, index: hit.Rule, size: hit.End - hit.Start}, hit.Start)
	})
}

func (r *maskInput) covered(hit secrets.ReaderHit, credentials []replacement) (bool, error) {
	for _, item := range credentials {
		if int64(len(item.from)) != hit.End-hit.Start {
			continue
		}
		equal, err := r.equal(hit.Start, item.from)
		if equal || err != nil {
			return equal, err
		}
	}
	return false, nil
}

func (h *readerHits) remember(key hitSize, start int64) error {
	for _, value := range h.seen[key] {
		equal, err := h.input.equal(start, value)
		if equal || err != nil {
			return err
		}
	}
	value, err := h.value(key, start)
	if err != nil {
		return err
	}
	h.seen[key] = append(h.seen[key], value)
	h.hits = append(h.hits, patternHit{family: key.family, index: key.index, value: value})
	return nil
}

func (h *readerHits) value(key hitSize, start int64) (string, error) {
	prefix := patternPrefix(key.family, key.index)
	for placeholder, value := range h.known {
		if int64(len(value)) != key.size || !strings.HasPrefix(placeholder, prefix) {
			continue
		}
		equal, err := h.input.equal(start, value)
		if err != nil {
			return "", err
		}
		if equal {
			return value, nil
		}
	}
	var value strings.Builder
	value.Grow(int(key.size))
	for n := int64(0); n < key.size; {
		window := h.input.scratch[:min(int64(maskReadBytes), key.size-n)]
		if err := h.input.read(window, start+n); err != nil {
			return "", err
		}
		_, _ = value.Write(window)
		n += int64(len(window))
	}
	return value.String(), nil
}

const placeholderExpression = `\{\{secret:(?:(?:(?:pattern|builtin)[0-9]+|keyword):[0-9]+|auth:[^\x00-\x20:{}]+(?::(?:access|refresh))?|(?:[A-Za-z_][A-Za-z_0-9]*)?_(?:API_KEY|TOKEN|SECRET|PASSWORD))\}\}`

type placeholderReader struct {
	matcher    *secrets.RegexpReader
	start, end int64
	exhausted  bool
}

func (r *placeholderReader) covers(input *maskInput, hit secrets.ReaderHit) (bool, error) {
	if r.matcher == nil {
		matcher, err := secrets.NewRegexpReader(input.ctx, input.source, placeholderExpression)
		if err != nil {
			return false, err
		}
		r.matcher = matcher
	}
	for !r.exhausted && r.end <= hit.Start {
		start, end, err := r.matcher.Next()
		if errors.Is(err, io.EOF) {
			r.exhausted = true
			break
		}
		if err != nil {
			return false, err
		}
		r.start, r.end = start, end
	}
	return r.start <= hit.Start && hit.End <= r.end, nil
}

type maskInput struct {
	ctx     context.Context
	source  *io.SectionReader
	scratch [maskReadBytes]byte
}

func (r *maskInput) read(dst []byte, offset int64) error {
	if err := context.Cause(r.ctx); err != nil {
		return err
	}
	n, err := r.source.ReadAt(dst, offset)
	if err != nil && !errors.Is(err, io.EOF) {
		return err
	}
	if n != len(dst) {
		return io.ErrUnexpectedEOF
	}
	return nil
}

func (r *maskInput) equal(offset int64, value string) (bool, error) {
	for len(value) > 0 {
		window := r.scratch[:min(maskReadBytes, len(value))]
		if err := r.read(window, offset); err != nil {
			return false, err
		}
		if !bytes.Equal(window, []byte(value[:len(window)])) {
			return false, nil
		}
		offset += int64(len(window))
		value = value[len(window):]
	}
	return true, nil
}

type maskSinks struct {
	raw, suffix io.Writer
	boundary    int64
}

func (r *maskInput) renderBlob(output io.Writer, plan []replacement) error {
	buffer := bufio.NewWriterSize(output, maskReadBytes)
	if err := r.writeMasked(maskSinks{raw: buffer, boundary: r.source.Size()}, plan); err != nil {
		return err
	}
	return buffer.Flush()
}

func (r *maskInput) writeMasked(output maskSinks, items []replacement) error {
	var bins [256][]replacement
	slices.SortFunc(items, compareReplacements)
	for _, item := range items {
		first := item.from[0]
		bins[first] = append(bins[first], item)
	}
	var block [maskReadBytes]byte
	for offset := int64(0); offset < r.source.Size(); {
		window := block[:min(maskReadBytes, r.source.Size()-offset)]
		if err := r.read(window, offset); err != nil {
			return err
		}
		advance, err := r.writeBlock(output, window, offset, &bins)
		if err != nil {
			return err
		}
		offset += int64(advance)
	}
	return nil
}

func (r *maskInput) writeBlock(out maskSinks, block []byte, offset int64, bins *[256][]replacement) (int, error) {
	written := 0
	for pos := 0; pos < len(block); {
		item, err := r.match(block[pos:], offset+int64(pos), bins[block[pos]])
		if err != nil {
			return 0, err
		}
		if item.from == "" {
			pos++
			continue
		}
		if err := out.writePiece(block[written:pos], offset+int64(written), item.to); err != nil {
			return 0, err
		}
		pos += len(item.from)
		written = pos
	}
	if written < len(block) {
		if err := out.writeBytes(offset+int64(written), block[written:]); err != nil {
			return 0, err
		}
	}
	return max(written, len(block)), nil
}

func (s maskSinks) writePiece(prefix []byte, offset int64, placeholder string) error {
	if err := s.writeBytes(offset, prefix); err != nil {
		return err
	}
	sink := s.raw
	if offset+int64(len(prefix)) >= s.boundary {
		sink = s.suffix
	}
	_, err := io.WriteString(sink, placeholder)
	return err
}

func (s maskSinks) writeBytes(offset int64, data []byte) error {
	n := int(min(int64(len(data)), max(s.boundary-offset, 0)))
	if n > 0 {
		if _, err := s.raw.Write(data[:n]); err != nil {
			return err
		}
	}
	if n < len(data) {
		_, err := s.suffix.Write(data[n:])
		return err
	}
	return nil
}

func (r *maskInput) match(block []byte, offset int64, items []replacement) (replacement, error) {
	for _, item := range items {
		if int64(len(item.from)) > r.source.Size()-offset {
			continue
		}
		n := min(len(item.from), len(block))
		if !bytes.Equal(block[:n], []byte(item.from[:n])) {
			continue
		}
		equal, err := r.equal(offset+int64(n), item.from[n:])
		if err != nil {
			return replacement{}, err
		}
		if equal {
			return item, nil
		}
	}
	return replacement{}, nil
}
