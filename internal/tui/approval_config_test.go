package tui

import (
	"context"
	"testing"

	"github.com/gosuda/dalgona/internal/rpc"
)

type policyCaller struct {
	fakeCaller
	policy string
}

func (f *policyCaller) Call(ctx context.Context, method string, params, result any) error {
	if out, ok := result.(*rpc.ThreadResumeResponse); ok {
		f.calls = append(f.calls, callRecord{method: method, params: params})
		out.ApprovalPolicy = f.policy
		out.Thread = rpc.Thread{ID: "t1", Name: "resumed"}
		return nil
	}
	return f.fakeCaller.Call(ctx, method, params, result)
}

func bandItem(m model, id string) string {
	left, right := m.bandSegments()
	for _, segment := range append(left, right...) {
		if segment.id == id {
			return segment.text
		}
	}
	return ""
}

func TestNewThreadLeavesApprovalToConfig(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{}, nil)
	m.startThread()()
	params, ok := caller.calls[0].params.(rpc.ThreadStartParams)
	if !ok || params.ApprovalPolicy != "" {
		t.Fatalf("thread/start approvalPolicy = %q, want empty so the configured approval applies", params.ApprovalPolicy)
	}
}

func TestNewThreadShowsServerApproval(t *testing.T) {
	t.Parallel()
	m := newTestModel(new(fakeCaller), Options{}, nil)
	m, _ = drive(t, m, resultMsg{op: "new", value: &rpc.ThreadStartResponse{ApprovalPolicy: "untrusted", Thread: rpc.Thread{ID: "t1"}}})
	if got := bandItem(m, "approval"); got != "?" {
		t.Fatalf("approval item = %q, want ? for the configured ask mode", got)
	}
}

func TestResumedThreadShowsServerApproval(t *testing.T) {
	t.Parallel()
	m := newTestModel(&policyCaller{policy: "never"}, Options{}, nil)
	m, _ = drive(t, m, m.resumeThread("t1")())
	if got := bandItem(m, "approval"); got != "!" {
		t.Fatalf("approval item = %q, want ! for the configured yolo mode", got)
	}
}
