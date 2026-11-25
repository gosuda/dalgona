package platform

import (
	"context"
	"io"

	"github.com/gosuda/dalgona/internal/dalgona/gui/clipboardimage"
)

type pngClipboardOffer struct {
	ctx     context.Context
	publish func(context.Context, []byte) error
	data    []byte
	written int
}

func NewPNGClipboardOffer(ctx context.Context, publish func(context.Context, []byte) error) clipboardimage.Offer {
	return &pngClipboardOffer{ctx: ctx, publish: publish}
}

func (*pngClipboardOffer) NeedsDIB() bool { return false }

func (p *pngClipboardOffer) Stage(spec clipboardimage.Spec) (io.Writer, io.Writer, error) {
	if err := p.ctx.Err(); err != nil {
		return nil, nil, err
	}
	if err := spec.Check(); err != nil || spec.DIBBytes != 0 || p.data != nil {
		return nil, nil, clipboardimage.ErrBounds
	}
	p.data = make([]byte, spec.PNGBytes)
	return p, nil, nil
}

func (p *pngClipboardOffer) Write(chunk []byte) (int, error) {
	if len(chunk) > len(p.data)-p.written {
		return 0, io.ErrShortWrite
	}
	n := copy(p.data[p.written:], chunk)
	p.written += n
	return n, nil
}

func (p *pngClipboardOffer) Publish() error {
	if err := p.ctx.Err(); err != nil {
		return err
	}
	if len(p.data) == 0 || p.written != len(p.data) {
		return io.ErrUnexpectedEOF
	}
	data := p.data
	p.Close()
	return p.publish(p.ctx, data)
}

func (p *pngClipboardOffer) Close() {
	p.data, p.written = nil, 0
}
