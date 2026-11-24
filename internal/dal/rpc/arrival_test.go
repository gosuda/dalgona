package rpc

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"net"
	"slices"
	"sync"
	"testing"
	"time"
)

type arrivalRecorder struct {
	mu       sync.Mutex
	events   []string
	notified chan struct{}
	release  chan struct{}
	answer   string
}

func newArrivalRecorder(answer string) *arrivalRecorder {
	return &arrivalRecorder{notified: make(chan struct{}, 8), release: make(chan struct{}), answer: answer}
}

func (r *arrivalRecorder) record(event string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, event)
}

func (r *arrivalRecorder) Notification(method string, _ jsontext.Value) {
	r.record("notified " + method)
	r.notified <- struct{}{}
}

func (r *arrivalRecorder) RequestArrived(method string, params jsontext.Value) {
	r.record(fmt.Sprintf("arrived %s %s", method, params))
}

func (r *arrivalRecorder) Request(ctx context.Context, method string, _ jsontext.Value) (any, error) {
	r.record("requested " + method)
	select {
	case <-r.release:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	return r.answer, nil
}

func (r *arrivalRecorder) log() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return slices.Clone(r.events)
}

func TestRequestArrivalPrecedesTheNotificationsSentAfterIt(t *testing.T) {
	near, far := net.Pipe()
	recorder := newArrivalRecorder("done")
	client := Dial(t.Context(), near, recorder)
	closeRPCPair(t, client, far)
	written := make(chan error, 1)
	go func() {
		_, err := far.Write([]byte(`{"jsonrpc":"2.0","id":"req-1","method":"test/ask","params":{"n":1}}` + "\n" + `{"jsonrpc":"2.0","method":"test/after"}` + "\n"))
		written <- err
	}()
	select {
	case <-recorder.notified:
	case <-time.After(waitLimit):
		t.Fatal("the notification after the request never arrived")
	}
	if err := <-written; err != nil {
		t.Fatal(err)
	}
	got := recorder.log()
	if slices.Index(got, "notified test/after") < 1 || got[0] != `arrived test/ask {"n":1}` {
		t.Fatalf("events %q, want the arrival of test/ask before notified test/after", got)
	}
	close(recorder.release)
	line, err := bufio.NewReader(far).ReadString('\n')
	if err != nil || line != `{"jsonrpc":"2.0","id":"req-1","result":"done"}`+"\n" {
		t.Fatalf("response %q, %v", line, err)
	}
}

func goodAnswersOnly(rejected chan<- error) ResponseCheck {
	return ResponseCheck{
		Accept: func(result jsontext.Value) error {
			if string(result) != `"good"` {
				return fmt.Errorf("answer %s is not good", result)
			}
			return nil
		},
		Reject: func(rejection error) { rejected <- rejection },
	}
}

func TestRespondSendsACorrectedAnswerForTheRejectedRequest(t *testing.T) {
	near, far := net.Pipe()
	recorder := newArrivalRecorder("bad")
	close(recorder.release)
	client := Dial(t.Context(), near, recorder)
	server := NewConn(t.Context(), far, nil)
	closeRPCPair(t, client, far)
	t.Cleanup(func() { _ = server.Close() })
	rejected := make(chan error, 4)
	check := goodAnswersOnly(rejected)
	outcome := make(chan error, 1)
	var result string
	go func() {
		outcome <- server.CallIDValidated(t.Context(), ValidatedCall{ID: "req-7", Method: "test/ask", Params: map[string]int{"n": 1}, Check: check}, &result)
	}()
	select {
	case rejection := <-rejected:
		if rejection == nil {
			t.Fatal("the rejection carried no reason")
		}
	case <-time.After(waitLimit):
		t.Fatal("the first answer was never rejected")
	}
	if err := client.Respond(t.Context(), "req-7", "good"); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-outcome:
		if err != nil || result != "good" {
			t.Fatalf("the corrected call ended with %q, %v", result, err)
		}
	case <-time.After(waitLimit):
		t.Fatal("the corrected answer never reached the waiting call")
	}
}

func TestRespondRefusesAnEndedContext(t *testing.T) {
	near, far := net.Pipe()
	client := Dial(t.Context(), near, newArrivalRecorder(""))
	closeRPCPair(t, client, far)
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if err := client.Respond(ctx, "req-1", "late"); !errors.Is(err, context.Canceled) {
		t.Fatalf("Respond after cancellation = %v, want context.Canceled", err)
	}
}
