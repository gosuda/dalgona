package rpc

import (
	"math"
)

type ThreadSearchParams struct {
	Query    string `json:"query"`
	Limit    int    `json:"limit,omitzero"`
	HasLimit bool   `json:"-"`
	Cursor   string `json:"cursor,omitzero"`
	Archived bool   `json:"archived,omitzero"`
	Cwd      string `json:"cwd,omitzero"`
}

type ThreadSearchRow struct {
	ThreadID  string `json:"threadId"`
	Name      string `json:"name"`
	Preview   string `json:"preview"`
	Cwd       string `json:"cwd"`
	UpdatedAt int64  `json:"updatedAt"`
	Seq       uint64 `json:"seq"`
	Kind      string `json:"kind"`
	Snippet   string `json:"snippet"`
	Matches   int    `json:"matches"`
}

type ThreadSearchResponse struct {
	Data       []ThreadSearchRow `json:"data"`
	NextCursor string            `json:"nextCursor,omitzero"`
}

func (r ThreadSearchRow) Encode(o *Writer) {
	o.Open()
	o.Str("threadId", r.ThreadID)
	o.Str("name", r.Name)
	o.Str("preview", r.Preview)
	o.Str("cwd", r.Cwd)
	o.Int("updatedAt", r.UpdatedAt)
	seq := r.Seq
	if seq > uint64(math.MaxInt64) {
		seq = uint64(math.MaxInt64)
	}
	o.Int("seq", int64(seq))
	o.Str("kind", r.Kind)
	o.Str("snippet", r.Snippet)
	o.Int("matches", int64(r.Matches))
	o.Close()
}

func (r ThreadSearchResponse) Encode(o *Writer) {
	o.Open()
	o.Key("data")
	o.OpenArray()
	for _, row := range r.Data {
		row.Encode(o)
	}
	o.CloseArray()
	o.OptStr("nextCursor", r.NextCursor)
	o.Close()
}
