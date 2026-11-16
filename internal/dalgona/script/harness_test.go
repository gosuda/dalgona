package script_test

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
	corescript "github.com/gosuda/dalgona/internal/script"
)

type toolFunc func(ctx context.Context, name string, args jsontext.Value) (jsontext.Value, error)

type judgeFunc func(ctx context.Context, req corescript.JudgeRequest) (jsontext.Value, error)

type journal struct {
	mu         sync.Mutex
	starts     map[string]corescript.RunStart
	records    map[string]map[int]corescript.Recorded
	tool       toolFunc
	judge      judgeFunc
	attempts   int
	executed   int
	judged     int
	killBefore int
	killDuring int
	kill       context.CancelFunc
	logs       []string
	completed  map[string]corescript.Outcome
}

func newJournal(fn toolFunc) *journal {
	return &journal{starts: map[string]corescript.RunStart{}, records: map[string]map[int]corescript.Recorded{}, tool: fn}
}

func (j *journal) Started(start corescript.RunStart) (corescript.RunStart, *corescript.Outcome, error) {
	j.mu.Lock()
	defer j.mu.Unlock()
	if pinned, ok := j.starts[start.ID]; ok {
		return pinned, nil, nil
	}
	j.starts[start.ID] = start
	return start, nil, nil
}

func (j *journal) Lookup(run string, n int) (corescript.Recorded, bool) {
	j.mu.Lock()
	defer j.mu.Unlock()
	rec, ok := j.records[run][n]
	return rec, ok
}

func (j *journal) put(slot corescript.Slot, rec corescript.Recorded) {
	j.mu.Lock()
	defer j.mu.Unlock()
	if j.records[slot.Run] == nil {
		j.records[slot.Run] = map[int]corescript.Recorded{}
	}
	j.records[slot.Run][slot.N] = rec
}

func (j *journal) Record(slot corescript.Slot, result jsontext.Value) error {
	j.put(slot, corescript.Recorded{Op: slot.Op, ArgsSHA: slot.ArgsSHA, Result: result})
	return nil
}

func (j *journal) Completed(run string, outcome corescript.Outcome) error {
	j.mu.Lock()
	defer j.mu.Unlock()
	if _, started := j.starts[run]; !started {
		return errors.New("completed a run that never started: " + run)
	}
	if j.completed == nil {
		j.completed = map[string]corescript.Outcome{}
	}
	j.completed[run] = outcome
	return nil
}

func (j *journal) attempt() int {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.attempts++
	return j.attempts
}

func (j *journal) CallTool(ctx context.Context, slot corescript.Slot, name string, args jsontext.Value) (jsontext.Value, error) {
	n := j.attempt()
	if n == j.killBefore {
		j.kill()
		return nil, ctx.Err()
	}
	j.put(slot, corescript.Recorded{Op: slot.Op, ArgsSHA: slot.ArgsSHA, Interrupted: true})
	if n == j.killDuring {
		j.kill()
		return nil, ctx.Err()
	}
	j.mu.Lock()
	j.executed++
	j.mu.Unlock()
	value, err := j.tool(ctx, name, args)
	rec := corescript.Recorded{Op: slot.Op, ArgsSHA: slot.ArgsSHA, Result: value}
	if err != nil {
		rec.Result = nil
		rec.Error = err.Error()
		rec.Denied = errors.Is(err, corescript.ErrDenied)
	}
	j.put(slot, rec)
	return value, err
}

func (j *journal) Spawn(_ context.Context, slot corescript.Slot, prompt string, _ corescript.SpawnOpts) (string, error) {
	id := "child-" + prompt
	value, err := jsontext.AppendQuote(nil, id)
	if err != nil {
		return "", err
	}
	j.put(slot, corescript.Recorded{Op: slot.Op, ArgsSHA: slot.ArgsSHA, Result: value})
	return id, nil
}

func (j *journal) Wait(_ context.Context, ids []string) ([]string, error) {
	texts := make([]string, len(ids))
	for i, id := range ids {
		texts[i] = "final:" + id
	}
	return texts, nil
}

func (j *journal) Judge(ctx context.Context, _ corescript.Slot, req corescript.JudgeRequest) (jsontext.Value, error) {
	j.mu.Lock()
	j.judged++
	j.mu.Unlock()
	return j.judge(ctx, req)
}

func (j *journal) Log(_, msg string) {
	j.mu.Lock()
	defer j.mu.Unlock()
	j.logs = append(j.logs, msg)
}

func echoTool(_ context.Context, name string, args jsontext.Value) (jsontext.Value, error) {
	return jsontext.Value(`{"tool":"` + name + `","args":` + string(args) + `}`), nil
}

type reply struct {
	InvocationID string         `json:"invocation_id"`
	Status       string         `json:"status"`
	Value        jsontext.Value `json:"value"`
	Error        *struct {
		Code    string `json:"code"`
		Message string `json:"message"`
		Stage   string `json:"stage"`
	} `json:"error"`
	Prints struct {
		Lines     []string `json:"lines"`
		Truncated bool     `json:"truncated"`
	} `json:"prints"`
}

func decodeReply(t *testing.T, result tool.Result) reply {
	t.Helper()
	var out reply
	if err := json.Unmarshal(result.Output, &out); err != nil {
		t.Fatalf("decode reply %s: %v", result.Output, err)
	}
	return out
}

func runEval(ctx context.Context, t *testing.T, eval *corescript.Eval, id, args string) reply {
	t.Helper()
	result, err := eval.Run(ctx, tool.Call{ID: id, Name: corescript.EvalName, Args: jsontext.Value(args)}, tool.Env{})
	if err != nil {
		t.Fatalf("eval run: %v", err)
	}
	got := decodeReply(t, result)
	if result.OK != (got.Status == "completed") {
		t.Fatalf("result OK = %v for status %s", result.OK, got.Status)
	}
	return got
}

func pluginSpecs(names ...string) []tool.Spec {
	out := make([]tool.Spec, len(names))
	for i, name := range names {
		out[i] = tool.Spec{Name: name, Description: "Run " + name + ".\nSecond line."}
	}
	return out
}

var pluginThreadTools = pluginSpecs("read", "search", "patch", "exec", "web_fetch", "task", "todo", "mcp__docs__lookup", "eval")
