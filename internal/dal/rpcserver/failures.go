package rpcserver

import (
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/failure"
)

const (
	failureListDefaultLimit = 50
	failureListMaxLimit     = 100
	failureCursorLifetime   = 5 * time.Minute
	failureIDLength         = 32
)

type failurePage struct {
	records []failure.Record
	next    string
}

func failureMethods() map[string]method {
	return map[string]method{
		"dalgona/failures/list": deadline(MetadataDeadline, (*server).failuresList),
		"dalgona/failures/read": deadline(MetadataDeadline, (*server).failuresRead),
	}
}

func (s *server) failuresList(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	now := time.Now()
	query, err := decodeFailureList(params, now)
	if err != nil {
		return nil, err
	}
	records, err := s.failureRecords(ctx, query)
	if err != nil {
		return nil, err
	}
	page := failurePage{records: records}
	if len(records) == query.Limit {
		last := records[len(records)-1]
		page.next = encodeFailureCursor(failure.Position{Time: last.Time, ID: last.ID}, now)
	}
	return rpc.Marshal(page)
}

func (s *server) failuresRead(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if name != "id" {
			return d.SkipValue()
		}
		value, readErr := rpc.ReadString(d)
		id = value
		return readErr
	})
	if err != nil || !failureID(id) {
		return nil, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	records, err := s.failureRecords(ctx, failure.Query{ID: id})
	if err != nil {
		return nil, err
	}
	encoded, err := json.Marshal(records[0])
	if err != nil {
		return nil, internalError(err)
	}
	return encoded, nil
}

func (s *server) failureRecords(ctx context.Context, query failure.Query) ([]failure.Record, error) {
	store, err := failure.Open(ctx, failure.Options{Home: s.host.Config().Home})
	if err != nil {
		return nil, failureStoreError(ctx, err)
	}
	records, err := store.List(ctx, query)
	if err != nil {
		return nil, failureStoreError(ctx, err)
	}
	return records, nil
}

func failureStoreError(ctx context.Context, err error) error {
	switch {
	case ctx.Err() != nil:
		return ctx.Err()
	case errors.Is(err, failure.ErrRecordNotFound):
		return rpc.NativeKind(rpc.ErrorNotFound)
	}
	return rpc.NativeKind(rpc.ErrorUnavailable)
}

func decodeFailureList(params jsontext.Value, now time.Time) (failure.Query, error) {
	query := failure.Query{Limit: failureListDefaultLimit}
	var cursor string
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var readErr error
		switch name {
		case "cursor":
			cursor, readErr = rpc.ReadString(d)
		case "limit":
			query.Limit, readErr = rpc.ReadInt(d)
		default:
			readErr = d.SkipValue()
		}
		return readErr
	})
	if err != nil || query.Limit < 1 || query.Limit > failureListMaxLimit {
		return query, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if cursor == "" {
		return query, nil
	}
	position, err := decodeFailureCursor(cursor, now)
	query.OlderThan = &position
	return query, err
}

func encodeFailureCursor(position failure.Position, issued time.Time) string {
	text := strconv.FormatInt(issued.UnixMilli(), 10) + "." + strconv.FormatInt(position.Time.UnixNano(), 10) + "." + position.ID
	return base64.RawURLEncoding.EncodeToString([]byte(text))
}

func decodeFailureCursor(cursor string, now time.Time) (failure.Position, error) {
	raw, err := base64.RawURLEncoding.DecodeString(cursor)
	parts := strings.Split(string(raw), ".")
	if err != nil || len(parts) != 3 || !failureID(parts[2]) {
		return failure.Position{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	issuedMs, issuedErr := strconv.ParseInt(parts[0], 10, 64)
	boundary, boundaryErr := strconv.ParseInt(parts[1], 10, 64)
	issued := time.UnixMilli(issuedMs)
	if errors.Join(issuedErr, boundaryErr) != nil || issued.After(now) {
		return failure.Position{}, rpc.NativeKind(rpc.ErrorInvalidParams)
	}
	if now.Sub(issued) > failureCursorLifetime {
		return failure.Position{}, rpc.NativeKind(rpc.ErrorExpired)
	}
	return failure.Position{Time: time.Unix(0, boundary).UTC(), ID: parts[2]}, nil
}

func failureID(id string) bool {
	return len(id) == failureIDLength && strings.Trim(id, "0123456789abcdef") == ""
}

func (p failurePage) Encode(o *rpc.Writer) {
	o.Open()
	o.Key("records")
	o.OpenArray()
	for _, record := range p.records {
		encodeFailureSummary(o, record)
	}
	o.CloseArray()
	o.OptStr("nextCursor", p.next)
	o.Close()
}

func encodeFailureSummary(o *rpc.Writer, record failure.Record) {
	o.Open()
	o.Str("id", record.ID)
	o.Str("time", rpc.FormatTimestamp(record.Time))
	o.Str("product", record.Product)
	o.Str("version", record.Version)
	o.Str("os", record.OS)
	o.Str("arch", record.Arch)
	o.Str("class", string(record.Class))
	o.Str("tool", record.Tool)
	o.Str("fingerprint", record.Fingerprint)
	o.Bool("contentFree", record.ContentFree)
	o.Bool("reportable", record.Reportable)
	o.Close()
}
