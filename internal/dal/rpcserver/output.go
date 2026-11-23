package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

func outputSpill(spill *event.Spill) *rpc.OutputSpill {
	if spill == nil {
		return nil
	}
	return &rpc.OutputSpill{Path: spill.Path, Bytes: spill.Bytes, SHA256: spill.SHA256}
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
	return rpc.Marshal(rpc.OutputReadResponse{Text: page.Text, Offset: page.Offset, Next: page.Next, Total: page.Total})
}

func outputReadParams(params jsontext.Value) (rpc.OutputReadParams, error) {
	var p rpc.OutputReadParams
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

func readOutputReadMember(name string, d *jsontext.Decoder, p *rpc.OutputReadParams) error {
	var err error
	switch name {
	case "threadId":
		p.ThreadID, err = rpc.ReadString(d)
	case "itemId":
		p.ItemID, err = rpc.ReadString(d)
	case "offset":
		var n int
		n, err = rpc.ReadInt(d)
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
