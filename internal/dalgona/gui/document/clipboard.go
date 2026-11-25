//go:build gui

package document

import (
	"context"
	"fmt"
	"strings"

	"gioui.org/io/input"

	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
)

func (s *Scope) CopyText(ctx context.Context, src input.Source, sel Selection, pager Pager) error {
	text, err := s.Copy(ctx, sel, pager)
	if err != nil {
		return err
	}
	return platform.WriteText(src, text)
}

func (s *Scope) CopyMarkdown(ctx context.Context, src input.Source, sel Selection, pager Pager) error {
	extent, err := s.Extent(sel)
	if err != nil {
		return err
	}
	size := extent.Bytes + 2*(extent.Documents-1)
	if size > MaxCopyBytes {
		return fmt.Errorf("%w: it exceeds %d bytes; use Save Markdown instead", ErrCopyLimit, MaxCopyBytes)
	}
	var text strings.Builder
	text.Grow(size)
	if err := s.WriteMarkdown(ctx, sel, pager, &text); err != nil {
		return err
	}
	return platform.WriteText(src, text.String())
}

func (d *Document) CopySource(src input.Source, block int) error {
	var source strings.Builder
	if err := d.WriteSource(block, &source); err != nil {
		return err
	}
	return platform.WriteText(src, source.String())
}
