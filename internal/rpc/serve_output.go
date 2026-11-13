package rpc

import (
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
)

func outputSpill(spill *event.Spill) *OutputSpill {
	if spill == nil {
		return nil
	}
	return &OutputSpill{Path: spill.Path, Bytes: spill.Bytes, SHA256: spill.SHA256}
}

func (s *server) itemOutputRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := outputReadParams(params)
	if err != nil {
		return nil, err
	}
	thread, err := s.open(ctx, p.ThreadID)
	if err != nil {
		return nil, err
	}
	page, err := thread.ReadOutput(ctx, p.ItemID, p.Offset)
	if err != nil {
		return nil, outputReadError(err, p.ThreadID)
	}
	return encode(OutputReadResponse{Text: page.Text, Offset: page.Offset, Next: page.Next, Total: page.Total})
}

func outputReadParams(params jsontext.Value) (OutputReadParams, error) {
	var p OutputReadParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		return readOutputReadMember(name, d, &p)
	})
	if err != nil {
		return p, err
	}
	if p.ItemID == "" {
		return p, invalidParams("itemId is required")
	}
	return p, nil
}

func readOutputReadMember(name string, d *jsontext.Decoder, p *OutputReadParams) error {
	var err error
	switch name {
	case "threadId":
		p.ThreadID, err = readString(d)
	case "itemId":
		p.ItemID, err = readString(d)
	case "offset":
		var n int
		n, err = readInt(d)
		p.Offset = int64(n)
	default:
		err = d.SkipValue()
	}
	return err
}

func outputReadError(err error, threadID string) error {
	_, offsetErr := errors.AsType[agent.OffsetError](err)
	if offsetErr || errors.Is(err, agent.ErrNoSpill) {
		return invalidParams(err.Error())
	}
	return hostError(err, threadID, "")
}

func (r OutputReadResponse) write(o *out) {
	o.open()
	o.str("text", r.Text)
	o.int("offset", r.Offset)
	o.int("next", r.Next)
	o.int("total", r.Total)
	o.close()
}
