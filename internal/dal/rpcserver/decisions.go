package rpcserver

import (
	"context"
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	contentDefaultLimit = 64 << 10
	contentMaxLimit     = 256 << 10
)

func decisionMethods() map[string]method {
	return map[string]method{
		"thread/unsubscribe":           (*server).threadUnsubscribe,
		"dalgona/request/details/read": deadline(MetadataDeadline, (*server).requestDetailsRead),
	}
}

type byteRange struct {
	offset int64
	limit  int64
}

func (r *byteRange) member(name string, d *jsontext.Decoder) (bool, error) {
	switch name {
	case "offset":
		text, err := rpc.ReadString(d)
		if err != nil {
			return true, err
		}
		offset, err := rpc.ParseDecimal(text)
		r.offset = int64(min(offset, 1<<62))
		return true, err
	case "limit":
		limit, err := rpc.ReadInt(d)
		r.limit = int64(limit)
		return true, err
	}
	return false, nil
}

func (r *byteRange) check(total int64) error {
	if r.limit == 0 {
		r.limit = contentDefaultLimit
	}
	if r.limit < 0 || r.limit > contentMaxLimit || r.offset > total {
		return rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return nil
}

func (s *server) threadUnsubscribe(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, rpc.ThreadIDMember(&id)); err != nil {
		return nil, err
	}
	if id == "" {
		return nil, invalidParams("threadId is required")
	}
	o := rpc.NewWriter()
	o.Open()
	o.Str("status", s.unsubscribe(id))
	o.Close()
	return o.Bytes()
}

func (s *server) unsubscribe(id string) string {
	s.mu.Lock()
	w := s.watches[id]
	s.mu.Unlock()
	if w == nil || !s.dropWatch(w) {
		if _, ok := s.info(id); ok {
			return "notSubscribed"
		}
		return "notLoaded"
	}
	w.stop()
	return "unsubscribed"
}

func (s *server) dropWatch(w *watch) bool {
	w.apply.Lock()
	defer w.apply.Unlock()
	id := w.thread.ID()
	s.mu.Lock()
	if s.watches[id] != w {
		s.mu.Unlock()
		return false
	}
	delete(s.watches, id)
	withdrawn := s.withdrawThreadLocked(id)
	if native := s.native.Load(); native != nil {
		native.histories.closeThread(id)
		native.contents.forgetThread(id)
	}
	s.mu.Unlock()
	for _, cancel := range withdrawn {
		cancel()
	}
	w.live.close()
	return true
}

func (s *server) withdrawThreadLocked(threadID string) []context.CancelFunc {
	var withdrawn []context.CancelFunc
	for requestID, owner := range s.threads {
		if owner == threadID {
			withdrawn = append(withdrawn, s.takeRequestLocked(requestID))
		}
	}
	return withdrawn
}

func decodeDetailsRequest(params jsontext.Value) (string, byteRange, error) {
	var requestID string
	var window byteRange
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if handled, memberErr := window.member(name, d); handled {
			return memberErr
		}
		if name != "requestId" {
			return d.SkipValue()
		}
		value, readErr := rpc.ReadString(d)
		requestID = value
		return readErr
	})
	if err != nil || requestID == "" || len(requestID) > rpc.IDLimit {
		return "", byteRange{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	return requestID, window, nil
}

func (s *server) requestDetailsRead(_ context.Context, params jsontext.Value) (jsontext.Value, error) {
	if s.native.Load() == nil {
		return nil, rpc.NativeKind(rpc.ErrorIncompatible)
	}
	requestID, window, err := decodeDetailsRequest(params)
	if err != nil {
		return nil, err
	}
	detail, ok := s.conn.PendingRequest(requestID)
	if !ok {
		return nil, rpc.NativeKind(rpc.ErrorNotFound)
	}
	if checkErr := window.check(int64(len(detail.Params))); checkErr != nil {
		return nil, checkErr
	}
	chunk, err := contentChunk(detail.Params, detail.Digest, window.offset, window.limit)
	if err != nil {
		return nil, err
	}
	return marshalChunk(chunk)
}

func marshalChunk(chunk rpc.ContentChunk) (jsontext.Value, error) {
	o := rpc.NewWriter()
	o.Open()
	o.Str("dataBase64", chunk.DataBase64)
	o.Str("offset", chunk.Offset)
	o.Str("next", chunk.Next)
	o.Str("total", chunk.Total)
	o.Str("digest", chunk.Digest)
	o.Close()
	return o.Bytes()
}
