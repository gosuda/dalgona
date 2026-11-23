package rpc

import (
	"encoding/json/jsontext"
)

func (p *ThreadTurnsListParams) Member(name string, d *jsontext.Decoder) error {
	var err error
	switch name {
	case "threadId":
		p.ThreadID, err = ReadString(d)
	case "cursor":
		p.Cursor, err = ReadString(d)
	case "limit":
		p.Limit, err = ReadInt(d)
	case "sortDirection":
		p.SortDirection, err = ReadString(d)
	case "itemsView":
		p.ItemsView, err = ReadString(d)
	default:
		err = d.SkipValue()
	}
	return err
}

func (r ThreadTurnsListResponse) Encode(o *Writer) {
	o.Open()
	o.Key("data")
	o.OpenArray()
	for _, turn := range r.Data {
		turn.Encode(o)
	}
	o.CloseArray()
	o.OptStr("nextCursor", r.NextCursor)
	o.OptStr("backwardsCursor", r.BackwardsCursor)
	o.Close()
}
