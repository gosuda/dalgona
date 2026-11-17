package rpc

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"math"
	"strconv"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/journal"
)

const (
	defaultSearchLimit = 20
	maxSearchLimit     = 100
)

type ThreadSearchParams struct {
	Query    string
	Limit    int
	HasLimit bool
	Cursor   string
	Archived bool
	Cwd      string
}

type ThreadSearchRow struct {
	ThreadID  string
	Name      string
	Preview   string
	Cwd       string
	UpdatedAt int64
	Seq       uint64
	Kind      string
	Snippet   string
	Matches   int
}

type ThreadSearchResponse struct {
	Data       []ThreadSearchRow
	NextCursor string
}

func (r ThreadSearchRow) write(o *out) {
	o.open()
	o.str("threadId", r.ThreadID)
	o.str("name", r.Name)
	o.str("preview", r.Preview)
	o.str("cwd", r.Cwd)
	o.int("updatedAt", r.UpdatedAt)
	seq := r.Seq
	if seq > uint64(math.MaxInt64) {
		seq = uint64(math.MaxInt64)
	}
	o.int("seq", int64(seq))
	o.str("kind", r.Kind)
	o.str("snippet", r.Snippet)
	o.int("matches", int64(r.Matches))
	o.close()
}

func (r ThreadSearchResponse) write(o *out) {
	o.open()
	o.key("data")
	o.openArray()
	for _, row := range r.Data {
		row.write(o)
	}
	o.closeArray()
	o.optStr("nextCursor", r.NextCursor)
	o.close()
}

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
	response := ThreadSearchResponse{Data: make([]ThreadSearchRow, 0, len(hits))}
	for _, hit := range hits {
		response.Data = append(response.Data, ThreadSearchRow{
			ThreadID: hit.Info.ID, Name: hit.Info.Name, Preview: hit.Info.Preview, Cwd: hit.Info.Cwd,
			UpdatedAt: unixSeconds(hit.Info.Updated), Seq: hit.Seq, Kind: hit.Kind, Snippet: hit.Snippet, Matches: hit.Matches,
		})
	}
	if more {
		response.NextCursor = strconv.Itoa(offset + len(hits))
	}
	return encode(response)
}

func decodeThreadSearchParams(params jsontext.Value) (ThreadSearchParams, error) {
	var p ThreadSearchParams
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "query":
			p.Query, err = readString(d)
		case "limit":
			p.Limit, err = readInt(d)
			p.HasLimit = err == nil
		case "cursor":
			p.Cursor, err = readString(d)
		case "archived":
			p.Archived, err = readBool(d)
		case "cwd":
			p.Cwd, err = readString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	return p, err
}

func searchLimit(p ThreadSearchParams) int {
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
