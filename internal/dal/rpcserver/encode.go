package rpcserver

import (
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type keywordList []rpc.Keyword

func (k keywordList) Encode(o *rpc.Writer) {
	o.OpenArray()
	for _, keyword := range k {
		o.Open()
		o.Str("word", keyword.Word)
		o.Key("hue")
		o.OpenArray()
		o.Token(jsontext.Int(int64(keyword.Hue[0])))
		o.Token(jsontext.Int(int64(keyword.Hue[1])))
		o.CloseArray()
		o.Close()
	}
	o.CloseArray()
}

type advisorList []rpc.AdvisorState

func (a advisorList) Encode(o *rpc.Writer) {
	o.OpenArray()
	for _, state := range a {
		o.Open()
		o.Str("name", state.Name)
		o.Str("model", state.Model)
		o.Str("status", state.Status)
		o.Int("notes", int64(state.Notes))
		o.Key("usage")
		o.Open()
		o.Int("inputTokens", int64(state.Usage.InputTokens))
		o.Int("outputTokens", int64(state.Usage.OutputTokens))
		o.Close()
		o.Close()
	}
	o.CloseArray()
}
