package rpc

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"net"
	"testing"
)

type auditCodedHandler struct{}

func (auditCodedHandler) Notification(string, jsontext.Value) {}
func (auditCodedHandler) Request(context.Context, string, jsontext.Value) (any, error) {
	return nil, &RPCError{Code: -32001, Message: "no runtime"}
}

func TestAuditClientRequestPreservesErrorCode(t *testing.T) {
	left, right := net.Pipe()
	client, err := Dial(right, auditCodedHandler{})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close()
	defer func() { _ = left.Close() }()
	if _, werr := left.Write([]byte(`{"id":1,"method":"test/method"}` + "\n")); werr != nil {
		t.Fatal(werr)
	}
	line, rerr := bufio.NewReader(left).ReadBytes('\n')
	if rerr != nil {
		t.Fatal(rerr)
	}
	var got struct {
		Error *wireError `json:"error"`
	}
	if uerr := json.Unmarshal(line, &got); uerr != nil {
		t.Fatal(uerr)
	}
	if got.Error == nil || got.Error.Code != -32001 {
		t.Fatalf("code=%+v, want -32001", got.Error)
	}
}
