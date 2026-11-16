package script_test

import (
	"context"
	"encoding/json/jsontext"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dalgona/script"
	corescript "github.com/gosuda/dalgona/internal/script"
)

const (
	hangHook    = "def hang(ctx, event):\n    time.sleep(3600)\non('tool_call', hang)\n"
	denyHook    = "def guard(ctx, event):\n    return {\"decision\": \"deny\", \"reason\": \"later hook denies\"}\non('tool_call', guard)\n"
	slowTool    = "def slow(ctx):\n    time.sleep(10)\n    return \"done\"\ntool('slow', 'Wait.', {'type': 'object'}, slow)\n"
	slowCell    = "{\"code\":\"ctx.sleep(10.0)\\n'done'\"}"
	hangTurnEnd = "def finish(ctx, event):\n    time.sleep(10)\non('turn_end', finish)\n"
)

const shortHookLimit = 50 * time.Millisecond

func sleepThenAllow(seconds string) string {
	return "def slow(ctx, event):\n    time.sleep(" + seconds + ")\n    return {\"decision\": \"allow\"}\non('tool_call', slow)\n"
}

func hookProbe() tool.Call {
	return tool.Call{ID: "c1", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`)}
}

type hookRun struct {
	decision script.HookDecision
	err      error
	elapsed  time.Duration
	journal  *journal
}

func runToolCallHooks(ctx context.Context, plugins *script.Plugins) hookRun {
	j := newJournal(echoTool)
	start := time.Now()
	decision, err := plugins.ToolCall(ctx, j, "c1", hookProbe())
	return hookRun{decision: decision, err: err, elapsed: time.Since(start), journal: j}
}

func requireHookTimeout(t *testing.T, got hookRun, message string, after time.Duration) {
	t.Helper()
	if got.err == nil || got.err.Error() != message {
		t.Fatalf("error = %v, want %q", got.err, message)
	}
	if got.elapsed != after {
		t.Fatalf("the hook ran %v, want %v", got.elapsed, after)
	}
}

func TestScriptToolCallHookDeadline(t *testing.T) {
	t.Run("a hung hook stops at the configured limit and gives no decision", hungHookStopsAtTheLimit)
	t.Run("a hook that ends inside the limit keeps its verdict without an error", quickHookKeepsItsVerdict)
	t.Run("a later denying hook still wins after an earlier hook timed out", laterDenialSurvivesATimeout)
	t.Run("the default limit is five seconds", defaultLimitIsFiveSeconds)
	t.Run("a shorter caller deadline is not reported as a hook timeout", callerDeadlineIsNotAHookTimeout)
}

func hungHookStopsAtTheLimit(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"hang.star": hangHook}).WithHookTimeout(shortHookLimit)
	synctest.Test(t, func(t *testing.T) {
		got := runToolCallHooks(t.Context(), plugins)
		requireHookTimeout(t, got, "plugin hang.star hook timed out after 50ms", shortHookLimit)
		if got.decision != (script.HookDecision{}) {
			t.Fatalf("decision = %+v, want none", got.decision)
		}
		if done := got.journal.completed["c1/hook:hang.star:hang"]; done.Status != corescript.StatusCancelled {
			t.Fatalf("hook run outcome = %+v, want cancelled", done)
		}
	})
}

func quickHookKeepsItsVerdict(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"slow.star": sleepThenAllow("0.04")}).WithHookTimeout(shortHookLimit)
	synctest.Test(t, func(t *testing.T) {
		got := runToolCallHooks(t.Context(), plugins)
		if got.err != nil || got.decision.Decision != script.DecisionAllow || got.decision.By != "hook:slow.star:slow" {
			t.Fatalf("decision = %+v, error = %v", got.decision, got.err)
		}
		if got.elapsed >= shortHookLimit {
			t.Fatalf("the hook ran %v, want under the limit", got.elapsed)
		}
	})
}

func laterDenialSurvivesATimeout(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"aa-hang.star": hangHook, "bb-deny.star": denyHook}).WithHookTimeout(shortHookLimit)
	synctest.Test(t, func(t *testing.T) {
		got := runToolCallHooks(t.Context(), plugins)
		requireHookTimeout(t, got, "plugin aa-hang.star hook timed out after 50ms", shortHookLimit)
		want := script.HookDecision{Decision: script.DecisionDeny, Reason: "later hook denies", By: "hook:bb-deny.star:guard"}
		if got.decision != want {
			t.Fatalf("decision = %+v, want %+v", got.decision, want)
		}
	})
}

func defaultLimitIsFiveSeconds(t *testing.T) {
	if script.HookTimeout != 5*time.Second {
		t.Fatalf("HookTimeout = %v, want 5s", script.HookTimeout)
	}
	hung := loadPlugins(t, map[string]string{"hang.star": hangHook})
	synctest.Test(t, func(t *testing.T) {
		requireHookTimeout(t, runToolCallHooks(t.Context(), hung), "plugin hang.star hook timed out after 5s", 5*time.Second)
	})
	patient := loadPlugins(t, map[string]string{"slow.star": sleepThenAllow("4")})
	synctest.Test(t, func(t *testing.T) {
		got := runToolCallHooks(t.Context(), patient)
		if got.err != nil || got.decision.Decision != script.DecisionAllow || got.elapsed != 4*time.Second {
			t.Fatalf("decision = %+v, error = %v, elapsed = %v", got.decision, got.err, got.elapsed)
		}
	})
}

func callerDeadlineIsNotAHookTimeout(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"hang.star": hangHook})
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithTimeout(t.Context(), 20*time.Millisecond)
		defer cancel()
		got := runToolCallHooks(ctx, plugins)
		if got.err == nil || strings.Contains(got.err.Error(), "hook timed out") || !strings.Contains(got.err.Error(), "context deadline exceeded") {
			t.Fatalf("error = %v, want the caller deadline", got.err)
		}
		if got.elapsed != 20*time.Millisecond {
			t.Fatalf("the hook ran %v, want 20ms", got.elapsed)
		}
	})
}

type timedReply struct {
	reply   reply
	elapsed time.Duration
}

func callSlowPluginTool(ctx context.Context, t *testing.T, plugins *script.Plugins) timedReply {
	t.Helper()
	tools := plugins.Tools(newJournal(echoTool))
	if len(tools) != 1 {
		t.Fatalf("plugin tools = %v", tools)
	}
	start := time.Now()
	result, err := tools[0].Run(ctx, tool.Call{ID: "call-1", Name: "slow", Args: jsontext.Value(`{}`)}, tool.Env{})
	if err != nil {
		t.Fatal(err)
	}
	return timedReply{reply: decodeReply(t, result), elapsed: time.Since(start)}
}

func callSlowCell(ctx context.Context, t *testing.T) timedReply {
	t.Helper()
	eval := corescript.NewEval(newJournal(echoTool), corescript.EvalOptions{Mode: config.ToolModeEvalOnly, Tools: pluginThreadTools})
	start := time.Now()
	got := runEval(ctx, t, eval, "call-1", slowCell)
	return timedReply{reply: got, elapsed: time.Since(start)}
}

func TestPluginToolAndEvalCellInheritTheCallDeadline(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"slow.star": slowTool}).WithHookTimeout(shortHookLimit)
	runs := map[string]func(context.Context, *testing.T) timedReply{
		"plugin tool": func(ctx context.Context, t *testing.T) timedReply { return callSlowPluginTool(ctx, t, plugins) },
		"eval cell":   callSlowCell,
	}
	for name, run := range runs {
		t.Run(name+" outlasts the hook limit under a longer call deadline", func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(t.Context(), 20*time.Second)
				defer cancel()
				got := run(ctx, t)
				if got.reply.Status != "completed" || string(got.reply.Value) != `"done"` || got.elapsed != 10*time.Second {
					t.Fatalf("reply = %+v, error = %+v, elapsed = %v", got.reply, got.reply.Error, got.elapsed)
				}
			})
		})
		t.Run(name+" stops at a shorter call deadline", func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
				defer cancel()
				got := run(ctx, t)
				if got.reply.Status != "cancelled" || got.elapsed != 2*time.Second {
					t.Fatalf("reply = %+v, error = %+v, elapsed = %v", got.reply, got.reply.Error, got.elapsed)
				}
			})
		})
	}
}

func TestTurnEndHookKeepsTheCallerDeadline(t *testing.T) {
	plugins := loadPlugins(t, map[string]string{"end.star": hangTurnEnd}).WithHookTimeout(shortHookLimit)
	payload := jsontext.Value(`{"turn":"turn-1"}`)
	synctest.Test(t, func(t *testing.T) {
		start := time.Now()
		if err := plugins.Emit(t.Context(), newJournal(echoTool), "turn-1/turn_end", script.EventTurnEnd, payload); err != nil {
			t.Fatalf("turn_end hook error = %v", err)
		}
		if got := time.Since(start); got != 10*time.Second {
			t.Fatalf("the turn_end hook ran %v, want 10s", got)
		}
	})
	synctest.Test(t, func(t *testing.T) {
		ctx, cancel := context.WithTimeout(t.Context(), 3*time.Second)
		defer cancel()
		start := time.Now()
		err := plugins.Emit(ctx, newJournal(echoTool), "turn-1/turn_end", script.EventTurnEnd, payload)
		if err == nil || strings.Contains(err.Error(), "hook timed out") || time.Since(start) != 3*time.Second {
			t.Fatalf("error = %v after %v, want the caller deadline at 3s", err, time.Since(start))
		}
	})
}
