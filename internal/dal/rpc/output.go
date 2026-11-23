package rpc

func (r OutputReadResponse) Encode(o *Writer) {
	o.Open()
	o.Str("text", r.Text)
	o.Int("offset", r.Offset)
	o.Int("next", r.Next)
	o.Int("total", r.Total)
	o.Close()
}
