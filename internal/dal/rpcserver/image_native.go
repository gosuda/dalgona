package rpcserver

import (
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"math"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/journal"
)

const (
	imageReadDefaultLimit = 256 << 10
	imageReadMaxLimit     = 1 << 20
)

type imageReadParams struct {
	threadID   string
	turnID     string
	itemID     string
	imageIndex int
	indexSet   bool
	offset     int64
	limit      int
}

type imageChunk struct {
	mimeType string
	digest   string
	total    int64
	offset   int64
	data     []byte
}

func imageMethods() map[string]method {
	return map[string]method{
		"dalgona/item/image/read": deadline(PageDeadline, (*server).itemImageRead),
	}
}

func (s *server) itemImageRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeImageRead(params)
	if err != nil {
		return nil, err
	}
	image, err := s.itemImage(ctx, p)
	if err != nil {
		return nil, err
	}
	data, total, err := s.host.BlobRange(&image.Blob, p.offset, p.limit)
	switch {
	case errors.Is(err, journal.ErrBlobRange):
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	case err != nil:
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	if total < 0 || p.offset < 0 {
		return nil, rpc.NativeKind(rpc.ErrorUnavailable)
	}
	return rpc.Marshal(imageChunk{mimeType: image.Mime, digest: image.Blob.Blob, total: total, offset: p.offset, data: data})
}

func decodeImageRead(params jsontext.Value) (imageReadParams, error) {
	p := imageReadParams{limit: imageReadDefaultLimit}
	if err := decodeParams(params, p.member); err != nil {
		return p, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if !p.tupleValid() || !p.indexSet || p.imageIndex < 0 || p.limit <= 0 || p.limit > imageReadMaxLimit {
		return p, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return p, nil
}

func (p *imageReadParams) tupleValid() bool {
	return !slices.ContainsFunc([]string{p.threadID, p.turnID, p.itemID}, func(id string) bool { return id == "" || len(id) > rpc.IDLimit })
}

func (p *imageReadParams) member(name string, d *jsontext.Decoder) error {
	var err error
	switch name {
	case "threadId":
		p.threadID, err = rpc.ReadString(d)
	case "turnId":
		p.turnID, err = rpc.ReadString(d)
	case "itemId":
		p.itemID, err = rpc.ReadString(d)
	case "imageIndex":
		p.imageIndex, err = rpc.ReadInt(d)
		p.indexSet = true
	case "offset":
		p.offset, err = readOffset(d)
	case "limit":
		p.limit, err = rpc.ReadInt(d)
	default:
		err = d.SkipValue()
	}
	return err
}

func readOffset(d *jsontext.Decoder) (int64, error) {
	text, err := rpc.ReadString(d)
	if err != nil {
		return 0, err
	}
	offset, err := rpc.ParseDecimal(text)
	return int64(min(offset, math.MaxInt64)), err
}

func (s *server) itemImage(ctx context.Context, p imageReadParams) (event.Image, error) {
	if _, ok := s.info(p.threadID); !ok {
		return event.Image{}, rpc.NativeKind(rpc.ErrorNotFound)
	}
	thread, err := s.host.Resume(ctx, p.threadID)
	if err != nil {
		return event.Image{}, hostError(err, p.threadID, p.turnID)
	}
	records, _, err := thread.Records()
	if err != nil {
		return event.Image{}, hostError(err, p.threadID, p.turnID)
	}
	if image, ok := p.find(records); ok {
		return image, nil
	}
	return event.Image{}, rpc.NativeKind(rpc.ErrorNotFound)
}

func (p *imageReadParams) find(records []event.Seqd) (event.Image, bool) {
	for _, record := range records {
		message, ok := record.Event.(event.UserMessage)
		if ok && message.TurnID == p.turnID && message.ItemID == p.itemID && p.imageIndex < len(message.Images) {
			return message.Images[p.imageIndex], true
		}
	}
	return event.Image{}, false
}

func (c imageChunk) Encode(o *rpc.Writer) {
	o.Open()
	o.Str("mimeType", c.mimeType)
	o.Str("digest", c.digest)
	o.Str("total", rpc.FormatDecimal(uint64(max(c.total, 0))))
	o.Str("offset", rpc.FormatDecimal(uint64(max(c.offset, 0))))
	o.Str("next", rpc.FormatDecimal(uint64(max(c.offset+int64(len(c.data)), 0))))
	o.Str("dataBase64", base64.StdEncoding.EncodeToString(c.data))
	o.Close()
}
