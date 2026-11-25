package state

import (
	"context"
	"encoding/json/jsontext"
	"slices"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	badAnswer  = `{"answers":{"q1":{"answers":["bad"]}}}`
	goodAnswer = `{"answers":{"q1":{"answers":["Postgres"]}}}`
)

func onlyRequest(t *testing.T, tv *ThreadView) RequestView {
	t.Helper()
	if len(tv.Requests) != 1 {
		t.Fatalf("%v lists requests %+v, want one", tv.Key, tv.Requests)
	}
	return tv.Requests[0]
}

func noticeTexts(v *View) []string {
	var out []string
	for _, n := range v.Notices() {
		out = append(out, n.Text)
	}
	return out
}

func warnAnswer(p *fakePort, request string) {
	link := p.link("local")
	p.event(link, "t1", "warning", map[string]string{
		"threadId": "t1", "requestId": request, "message": "invalid text: answers.q1.answers[0]: disallowed_control",
		"reason": "disallowed_control", "field": "answers.q1.answers[0]",
	})
}

func expectCorrections(t *testing.T, p *fakePort, want ...string) {
	t.Helper()
	var got []string
	for _, call := range p.calls(methodRespond) {
		c := call.params.(correction)
		got = append(got, c.request+" "+string(c.result.(jsontext.Value)))
	}
	if !slices.Equal(got, want) {
		t.Fatalf("corrections %q, want %q", got, want)
	}
}

func TestCorrectionWarningReopensTheAnswerUntilTheRequestResolves(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
		synctest.Wait()
		link := p.link("local")
		replies := p.request(link, p.next(link), "t1", "req-1")
		key := RequestKey{Thread: localThread, Request: "req-1"}
		synctest.Wait()
		p.owner.Submit(Answer{Request: key, Result: jsontext.Value(badAnswer)})
		expectAnswer(t, replies, badAnswer)
		warnAnswer(p, "req-1")
		warnAnswer(p, "req-resolved-earlier")
		tv := threadView(t, p, localThread)
		if rq := onlyRequest(t, tv); !correctable(rq) || tv.Warning != "" {
			t.Fatalf("after the warning the request is %+v and the thread warning %q", rq, tv.Warning)
		}
		p.owner.Submit(Answer{Request: key, Result: jsontext.Value(goodAnswer)})
		p.owner.Submit(Answer{Request: key, Result: jsontext.Value(badAnswer)})
		tv = threadView(t, p, localThread)
		expectCorrections(t, p, "req-1 "+goodAnswer)
		if rq := onlyRequest(t, tv); !rq.Answered || rq.Actionable || rq.Warning != nil {
			t.Fatalf("after the correction the request is %+v", rq)
		}
		p.event(link, "t1", methodResolved, rpc.ServerRequestResolvedNotification{ThreadID: "t1", RequestID: "req-1", Resolution: resolutionAnswered})
		tv = threadView(t, p, localThread)
		want := []ResolvedRequest{{Key: key, Method: "item/commandExecution/requestApproval", Resolution: resolutionAnswered, AnsweredHere: true}}
		if !slices.Equal(tv.Resolved, want) || len(tv.Requests) != 0 || len(settled(p).Notices()) != 1 {
			t.Fatalf("after resolution: requests %+v, resolved %+v, notices %q", tv.Requests, tv.Resolved, noticeTexts(settled(p)))
		}
	})
}

func correctable(rq RequestView) bool {
	return !rq.Answered && rq.Actionable && rq.Warning != nil && rq.Warning.Field == "answers.q1.answers[0]"
}

func TestResolutionsFromElsewhereAnnounceThemselvesAndCancelTheWait(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
		synctest.Wait()
		link := p.link("local")
		other := p.request(link, p.next(link), "t1", "req-other")
		late := p.request(link, p.next(link), "t1", "req-late")
		synctest.Wait()
		p.event(link, "t1", methodResolved, rpc.ServerRequestResolvedNotification{ThreadID: "t1", RequestID: "req-other", Resolution: resolutionAnswered})
		p.event(link, "t1", methodResolved, rpc.ServerRequestResolvedNotification{ThreadID: "t1", RequestID: "req-late", Resolution: resolutionTimedOut})
		v := settled(p)
		want := []string{"Another client answered a request on local", "Request timed out"}
		if got := noticeTexts(v); !slices.Equal(got, want) || v.Notices()[0].Tone != NoticeStatus {
			t.Fatalf("notices %q (%+v), want %q as status", got, v.Notices(), want)
		}
		for _, replies := range []<-chan answer{other, late} {
			if got := <-replies; got.err == nil {
				t.Fatalf("a request resolved elsewhere was answered with %v, %v", got.result, got.err)
			}
		}
	})
}

func TestChildRequestsShowInTheOpenParentAndAnswerThroughIt(t *testing.T) {
	withOwner(t, func(t *testing.T, p *fakePort) {
		p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
		synctest.Wait()
		link := p.link("local")
		p.event(link, "t1", "dalgona/thread/childSpawned", rpc.ChildSpawnedNotification{ThreadID: "t1", ChildID: "child-1"})
		replies := p.request(link, p.next(link), "child-1", "req-child")
		tv := threadView(t, p, localThread)
		rq := onlyRequest(t, tv)
		child := RequestKey{Thread: ThreadKey{Endpoint: "local", Thread: "child-1"}, Request: "req-child"}
		if rq.Key != child || !rq.Actionable {
			t.Fatalf("the parent lists %+v, want the actionable child request %v", rq, child)
		}
		p.owner.Submit(Answer{Request: child, Result: jsontext.Value(`{"decision":"accept"}`)})
		expectAnswer(t, replies, `{"decision":"accept"}`)
	})
}

func TestRequestAnsweredBeforeTheTransportWaitsStillReplies(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		runOwner(t, t.TempDir(), func(t *testing.T, p *fakePort) {
			p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
			synctest.Wait()
			link := p.link("local")
			header := requestHeader(p, link, p.next(link), "t1", "req-early")
			out := make(chan answer, 1)
			go func() {
				ctx, cancel := context.WithTimeout(t.Context(), time.Second)
				defer cancel()
				result, err := p.owner.Requested(ctx, link, rpc.MethodRequest, header)
				out <- answer{result: result, err: err}
			}()
			synctest.Wait()
			p.owner.Submit(Answer{Request: RequestKey{Thread: localThread, Request: "req-early"}, Result: jsontext.Value(`{"decision":"accept"}`)})
			expectAnswer(t, out, `{"decision":"accept"}`)
		})
	})
}
