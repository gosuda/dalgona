package rpc

type ReviewStartResponse struct {
	ReviewThreadID string `json:"reviewThreadId"`
	Turn           Turn   `json:"turn"`
}

func (r ReviewStartResponse) Encode(o *Writer) {
	o.Open()
	o.Str("reviewThreadId", r.ReviewThreadID)
	o.Key("turn")
	r.Turn.Encode(o)
	o.Close()
}
