package script_test

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
	"math/rand/v2"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/script"
)

type concurrencyProbe struct {
	mu      sync.Mutex
	rng     *rand.Rand
	active  atomic.Int32
	maxSeen atomic.Int32
}

func (p *concurrencyProbe) delay() time.Duration {
	p.mu.Lock()
	defer p.mu.Unlock()
	return time.Duration(p.rng.IntN(4000)) * time.Microsecond
}

func (p *concurrencyProbe) enter() func() {
	now := p.active.Add(1)
	for {
		seen := p.maxSeen.Load()
		if now <= seen || p.maxSeen.CompareAndSwap(seen, now) {
			break
		}
	}
	return func() { p.active.Add(-1) }
}

func TestScopeReturnsSubmissionOrderUnderRandomCompletion(t *testing.T) {
	for seed := range uint64(5) {
		synctest.Test(t, func(t *testing.T) { scopeSubmissionOrder(t, seed) })
	}
}

func scopeSubmissionOrder(t *testing.T, seed uint64) {
	probe := &concurrencyProbe{rng: rand.New(rand.NewPCG(seed, seed+1))}
	j := newJournal(func(_ context.Context, _ string, args jsontext.Value) (jsontext.Value, error) {
		defer probe.enter()()
		time.Sleep(probe.delay())
		return args, nil
	})
	src := "s = ctx.scope(limit = 4)\nfor i in range(24):\n    s.tools.echo(i = i)\n[r['i'] for r in s.all()]\n"
	outcome, err := script.RunWorkflow(t.Context(), j, script.Workflow{ID: "scope", Entry: "s.star", Source: []byte(src)})
	if err != nil {
		t.Fatal(err)
	}
	var got []int
	if err := json.Unmarshal(outcome.Value, &got); err != nil {
		t.Fatalf("seed %d: outcome %+v: %v", seed, outcome, err)
	}
	want := make([]int, 24)
	for i := range want {
		want[i] = i
	}
	if !slices.Equal(got, want) {
		t.Fatalf("seed %d: results %v, want submission order", seed, got)
	}
	if peak := probe.maxSeen.Load(); peak > 4 || peak < 2 {
		t.Fatalf("seed %d: peak concurrency %d, want 2..4", seed, peak)
	}
	for n := 1; n <= 24; n++ {
		if rec, _ := j.Lookup("scope", n); string(rec.Result) != fmt.Sprintf(`{"i":%d}`, n-1) {
			t.Fatalf("seed %d: effect %d recorded %s; indexes must follow submission", seed, n, rec.Result)
		}
	}
}

func TestScopeRejectsLimitOutsideRange(t *testing.T) {
	for _, limit := range []int{0, 65} {
		src := fmt.Sprintf("ctx.scope(limit = %d)", limit)
		outcome, err := script.RunWorkflow(t.Context(), newJournal(echoTool), script.Workflow{ID: "s", Entry: "s.star", Source: []byte(src)})
		if err != nil || outcome.Status != script.StatusFailed {
			t.Fatalf("limit %d: %v %+v", limit, err, outcome)
		}
	}
}

const judgeBatchWorkflow = `
q = {"risky": {"type": "choice", "instructions": "classify", "criteria": {"yes": "risky", "no": "safe"}}, "ok": {"type": "bool"}}
out = ctx.judge_batch(["a", "bad", {"text": "c"}, ["d"], 7], q, concurrency = 3)
[[r.answers["risky"].choice if r.answers else None, r.answers["ok"].noul if r.answers else None, r.error] for r in out]
`

func judgeByState(probe *concurrencyProbe) judgeFunc {
	return func(_ context.Context, req script.JudgeRequest) (jsontext.Value, error) {
		time.Sleep(probe.delay())
		if string(req.State) == `"bad"` {
			return nil, errors.New("judge refused")
		}
		var questions map[string]map[string]any
		if err := json.Unmarshal(req.Questions, &questions); err != nil || questions["ok"]["type"] != "noul" {
			return nil, fmt.Errorf("questions %s: bool alias must become noul", req.Questions)
		}
		choice, err := jsontext.AppendQuote(nil, string(req.State))
		if err != nil {
			return nil, err
		}
		return jsontext.Value(`{"risky":{"choice":` + string(choice) + `,"probabilities":{"yes":0.6},"confidence":0.6},"ok":{"noul":0.25}}`), nil
	}
}

func TestJudgeBatchKeepsInputOrderAndItemErrors(t *testing.T) {
	synctest.Test(t, judgeBatchOrder)
}

func judgeBatchOrder(t *testing.T) {
	probe := &concurrencyProbe{rng: rand.New(rand.NewPCG(7, 9))}
	j := newJournal(echoTool)
	j.judge = judgeByState(probe)
	run := func() []any {
		outcome, err := script.RunWorkflow(t.Context(), j, script.Workflow{ID: "judge-1", Entry: "j.star", Source: []byte(judgeBatchWorkflow)})
		if err != nil {
			t.Fatal(err)
		}
		return workflowValues(t, outcome)
	}
	first := run()
	want := `[["\"a\"",0.25,null],[null,null,"judge refused"],["{\"text\":\"c\"}",0.25,null],["[\"d\"]",0.25,null],[null,null,"state must be a string, dict, or list"]]`
	if got, _ := json.Marshal(first); string(got) != want {
		t.Fatalf("batch = %s\nwant    %s", got, want)
	}
	if _, judged := j.counts(); judged != 4 {
		t.Fatalf("judged %d items, want 4", judged)
	}

	second := run()
	if _, judged := j.counts(); judged != 5 {
		t.Fatalf("replay judged %d items in total; settled items must replay and only the failed one is asked again", judged)
	}
	if fmt.Sprint(second) != fmt.Sprint(first) {
		t.Fatalf("replay = %v, want %v", second, first)
	}
}

func TestSpawnAndWaitReplayChildIDs(t *testing.T) {
	j := newJournal(echoTool)
	src := []byte("ids = [ctx.spawn('one'), ctx.spawn('two', model = None)]\nctx.log('spawned')\nctx.wait(ids)\n")
	outcome, err := script.RunWorkflow(t.Context(), j, script.Workflow{ID: "kids", Entry: "k.star", Source: src})
	if err != nil {
		t.Fatal(err)
	}
	if string(outcome.Value) != `["final:child-one","final:child-two"]` {
		t.Fatalf("outcome = %+v", outcome)
	}
	if rec, _ := j.Lookup("kids", 2); rec.Op != script.OpSpawn || string(rec.Result) != `"child-two"` {
		t.Fatalf("spawn effect 2 = %+v", rec)
	}
	if len(j.logs) != 1 || j.logs[0] != "spawned" {
		t.Fatalf("logs = %v", j.logs)
	}
}
