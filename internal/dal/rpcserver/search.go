package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"strconv"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/journal"
)

const (
	defaultSearchLimit = 20
	maxSearchLimit     = 100
)

func (s *server) threadSearch(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	p, err := decodeThreadSearchParams(params)
	if err != nil {
		return nil, err
	}
	if p.Query == "" {
		return nil, invalidParams("query is required")
	}
	if p.Limit < 0 {
		return nil, invalidParams("invalid limit " + strconv.Itoa(p.Limit))
	}
	offset, err := cursorOffset(p.Cursor)
	if err != nil {
		return nil, err
	}
	hits, more, err := s.host.SearchThreads(ctx, agent.ThreadSearch{
		Text: p.Query, Cwd: p.Cwd, Archived: p.Archived, Limit: searchLimit(p), Offset: offset,
	})
	if err != nil {
		return nil, searchError(err)
	}
	response := rpc.ThreadSearchResponse{Data: make([]rpc.ThreadSearchRow, 0, len(hits))}
	for _, hit := range hits {
		response.Data = append(response.Data, rpc.ThreadSearchRow{
			ThreadID: hit.Info.ID, Name: hit.Info.Name, Preview: hit.Info.Preview, Cwd: hit.Info.Cwd,
			UpdatedAt: unixSeconds(hit.Info.Updated), Seq: hit.Seq, Kind: hit.Kind, Snippet: hit.Snippet, Matches: hit.Matches,
		})
	}
	if more {
		response.NextCursor = strconv.Itoa(offset + len(hits))
	}
	return rpc.Marshal(response)
}

func decodeThreadSearchParams(params jsontext.Value) (rpc.ThreadSearchParams, error) {
	var p rpc.ThreadSearchParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "query":
			p.Query, err = rpc.ReadString(d)
		case "limit":
			p.Limit, err = rpc.ReadInt(d)
			p.HasLimit = err == nil
		case "cursor":
			p.Cursor, err = rpc.ReadString(d)
		case "archived":
			p.Archived, err = rpc.ReadBool(d)
		case "cwd":
			p.Cwd, err = rpc.ReadString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return p, err
}

func searchLimit(p rpc.ThreadSearchParams) int {
	if !p.HasLimit || p.Limit <= 0 {
		return defaultSearchLimit
	}
	return min(p.Limit, maxSearchLimit)
}

func searchError(err error) error {
	if errors.Is(err, journal.ErrQueryShort) {
		return invalidParams("query must hold at least 3 characters")
	}
	return internalError(err)
}
