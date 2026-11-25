package secrets

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"io"
	"slices"
)

type ReaderHit struct {
	Rule       int
	Start, End int64
}

func ScanReader(ctx context.Context, input *io.SectionReader, accept func(ReaderHit) error) error {
	readers, err := builtinReaders(ctx, input)
	if err != nil {
		return err
	}
	var end int64
	for len(readers) != 0 {
		first := 0
		for i := 1; i < len(readers); i++ {
			if compareReaderHits(readers[i].hit, readers[first].hit) < 0 {
				first = i
			}
		}
		if err := emitReaderHit(accept, readers[first].hit, &end); err != nil {
			return err
		}
		err := readers[first].advance(ctx, input)
		if errors.Is(err, io.EOF) {
			readers = slices.Delete(readers, first, first+1)
		} else if err != nil {
			return err
		}
	}
	return nil
}

func emitReaderHit(accept func(ReaderHit) error, hit ReaderHit, end *int64) error {
	if hit.Start < *end {
		if hit.End <= *end {
			return nil
		}
		hit.Start = *end
	}
	*end = hit.End
	return accept(hit)
}

func compareReaderHits(a, b ReaderHit) int {
	return cmp.Or(cmp.Compare(a.Start, b.Start), cmp.Compare(b.End, a.End), cmp.Compare(a.Rule, b.Rule))
}

type builtinReader struct {
	matcher *RegexpReader
	hit     ReaderHit
}

func builtinReaders(ctx context.Context, input *io.SectionReader) ([]builtinReader, error) {
	present, err := presentNeedles(ctx, input)
	if err != nil {
		return nil, err
	}
	var readers []builtinReader
	for index, rule := range rules {
		if !present[index] {
			continue
		}
		matcher, err := NewRegexpReader(ctx, input, rule.re.String())
		if err != nil {
			return nil, err
		}
		reader := builtinReader{matcher: matcher, hit: ReaderHit{Rule: index}}
		err = reader.advance(ctx, input)
		if errors.Is(err, io.EOF) {
			continue
		}
		if err != nil {
			return nil, err
		}
		readers = append(readers, reader)
	}
	return readers, nil
}

func presentNeedles(ctx context.Context, input *io.SectionReader) ([]bool, error) {
	var block [scanBlockBytes]byte
	present := make([]bool, len(rules))
	step := int64(len(block) - longestNeedle() + 1)
	for offset := int64(0); offset < input.Size(); offset += step {
		window := block[:min(int64(len(block)), input.Size()-offset)]
		if err := readScanBlock(ctx, input, window, offset); err != nil {
			return nil, err
		}
		for i, rule := range rules {
			present[i] = present[i] || slices.ContainsFunc(rule.needles, func(needle string) bool {
				return bytes.Contains(window, []byte(needle))
			})
		}
	}
	return present, nil
}

func longestNeedle() int {
	longest := 1
	for _, rule := range rules {
		for _, needle := range rule.needles {
			longest = max(longest, len(needle))
		}
	}
	return longest
}

func (r *builtinReader) advance(ctx context.Context, input *io.SectionReader) error {
	for {
		start, end, err := r.matcher.Next()
		if err != nil {
			return err
		}
		r.hit.Start, r.hit.End = start, end
		if rules[r.hit.Rule].typ != openAIKeyType {
			return nil
		}
		candidate, err := openAIReaderKey(ctx, input, r.hit)
		if err != nil || candidate {
			return err
		}
	}
}

func openAIReaderKey(ctx context.Context, input *io.SectionReader, hit ReaderHit) (bool, error) {
	var block [scanBlockBytes]byte
	interiorDash := false
	for offset := hit.Start; offset < hit.End; {
		window := block[:min(int64(len(block)), hit.End-offset)]
		if err := readScanBlock(ctx, input, window, offset); err != nil {
			return false, err
		}
		if offset == hit.Start {
			if knownOpenAIKeyPrefix(string(window[:min(len(window), len("sk-svcacct-"))])) {
				return true, nil
			}
			window = window[len("sk-"):]
			offset += int64(len("sk-"))
		}
		if bytes.ContainsAny(window, "0123456789") {
			return true, nil
		}
		interiorDash = interiorDash || bytes.ContainsRune(window, '-')
		offset += int64(len(window))
	}
	return !interiorDash, nil
}

func readScanBlock(ctx context.Context, input *io.SectionReader, dst []byte, offset int64) error {
	if err := context.Cause(ctx); err != nil {
		return err
	}
	n, err := input.ReadAt(dst, offset)
	if err != nil && !errors.Is(err, io.EOF) {
		return err
	}
	if n != len(dst) {
		return io.ErrUnexpectedEOF
	}
	return nil
}
