package rpc

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"net"
	"os"
	"testing"
	"time"
)

type retainedHandler struct{ answers chan string }

func (retainedHandler) Notification(string, jsontext.Value) {}

func (h retainedHandler) Request(ctx context.Context, _ string, _ jsontext.Value) (any, error) {
	respond := ClientResponder(ctx)
	for {
		select {
		case answer := <-h.answers:
			if err := respond(map[string]string{"answer": answer}); err != nil {
				return nil, err
			}
		case <-ctx.Done():
			return nil, ErrResponded
		}
	}
}

type responderReply struct {
	ID     string `json:"id"`
	Result struct {
		Answer string `json:"answer"`
	} `json:"result"`
	Error *wireError `json:"error"`
}

func readResponderReply(t *testing.T, r *bufio.Reader) responderReply {
	t.Helper()
	line, err := r.ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	var got responderReply
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	return got
}

func TestClientResponderSendsEachAnswerAndNothingAfterResolution(t *testing.T) {
	left, right := net.Pipe()
	handler := retainedHandler{answers: make(chan string, 1)}
	client := Dial(t.Context(), right, handler)
	closeRPCPair(t, client, left)
	if err := left.SetDeadline(time.Now().Add(waitLimit)); err != nil {
		t.Fatal(err)
	}
	reader := bufio.NewReader(left)
	send(t, left, `{"id":"q-1","method":"item/tool/requestUserInput","params":{}}`)
	for _, answer := range []string{"bad", "good"} {
		handler.answers <- answer
		if got := readResponderReply(t, reader); got.ID != "q-1" || got.Result.Answer != answer || got.Error != nil {
			t.Fatalf("reply %+v, want answer %q for q-1", got, answer)
		}
	}
	send(t, left, `{"method":"serverRequest/resolved","params":{"threadId":"t","requestId":"q-1","resolution":"answered"}}`)
	if err := left.SetReadDeadline(time.Now().Add(200 * time.Millisecond)); err != nil {
		t.Fatal(err)
	}
	if line, err := reader.ReadBytes('\n'); !errors.Is(err, os.ErrDeadlineExceeded) {
		t.Fatalf("after resolution the client wrote %s (%v), want nothing", line, err)
	}
}

func TestClientResponderOutsideARequestFails(t *testing.T) {
	if err := ClientResponder(t.Context())(map[string]string{}); err == nil {
		t.Fatal("a responder outside a server request sent a response")
	}
}
