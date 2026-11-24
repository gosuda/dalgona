package client

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"
	"testing"
	"testing/synctest"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func (r *recorder) Arrived(link Link, method string, _ jsontext.Value) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, fmt.Sprintf("%s#%d arrived %s", link.Endpoint, link.Generation, method))
}

type answeringRecorder struct {
	*recorder
	answer string
}

func (r answeringRecorder) Requested(context.Context, Link, string, jsontext.Value) (any, error) {
	return r.answer, nil
}

func (d *fakeDaemon) current() *rpc.Conn {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.conns[len(d.conns)-1]
}

func onlyGood(rejected chan<- error) rpc.ResponseCheck {
	return rpc.ResponseCheck{
		Accept: func(result jsontext.Value) error {
			if string(result) != `"good"` {
				return errors.New("not good")
			}
			return nil
		},
		Reject: func(err error) { rejected <- err },
	}
}

func TestArrivalAndCorrectionStayOnTheirLink(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		rec := answeringRecorder{recorder: newRecorder(), answer: "bad"}
		daemon := newFakeDaemon()
		defer close(daemon.hold)
		set := openSet(t, rec, EndpointSpec{ID: "work", Label: "work", Kind: EndpointDaemon, Dial: daemon.Dial})
		if err := set.Connect("work"); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		link := rec.link(t, "work")
		rejected := make(chan error, 2)
		check := onlyGood(rejected)
		var result string
		done := make(chan error, 1)
		go func() {
			done <- daemon.current().CallIDValidated(t.Context(), rpc.ValidatedCall{ID: "req-1", Method: "test/ask", Check: check}, &result)
		}()
		synctest.Wait()
		if len(rejected) != 1 || !slices.Contains(rec.eventLog(), "work#1 arrived test/ask") {
			t.Fatalf("after the first answer: %d rejections, events %q", len(rejected), rec.eventLog())
		}
		stale := Link{Endpoint: link.Endpoint, Generation: link.Generation + 1}
		if err := set.Respond(t.Context(), stale, "req-1", "good"); !errors.Is(err, ErrStaleLink) {
			t.Fatalf("a correction on another link = %v, want ErrStaleLink", err)
		}
		if err := set.Respond(t.Context(), link, "req-1", "good"); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		if err := <-done; err != nil || result != "good" {
			t.Fatalf("the corrected request ended with %q, %v", result, err)
		}
	})
}
