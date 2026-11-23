package rpc

func (img ItemImage) Encode(o *Writer) {
	o.Open()
	o.Str("type", img.Type)
	o.Str("url", img.URL)
	o.Str("mime", img.Mime)
	o.Int("bytes", img.Bytes)
	o.OptStr("path", img.Path)
	o.Close()
}
