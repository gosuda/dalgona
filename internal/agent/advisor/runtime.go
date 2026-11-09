package advisor

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"maps"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/read"
	"github.com/gosuda/dalgona/internal/tool/search"
	"github.com/gosuda/dalgona/internal/tool/webfetch"
)

type State string

const (
	StateIdle      State = "idle"
	StateReviewing State = "reviewing"
	StateHalted    State = "halted"
	StateDisabled  State = "disabled"
)

type Status struct {
	Name  string
	Model string
	State State
	Notes int
	Usage event.Usage
}

var investigative = map[string]func() tool.Tool{
	"read":      read.New,
	"search":    search.New,
	"exec":      func() tool.Tool { return exec.New() },
	"web_fetch": webfetch.New,
}

var errRuntimeClosed = errors.New("advisor: runtime closed")

type Runtime struct {
	primary  *agent.Thread
	policy   Policy
	stop     context.CancelCauseFunc
	done     <-chan struct{}
	workers  sync.WaitGroup
	advisors []*advisor

	mu         sync.Mutex
	steered    bool
	sinceSteer int
}

func Attach(ctx context.Context, host *agent.Host, thread *agent.Thread, cfg config.Config) (*Runtime, error) {
	info, ok := threadInfo(host, thread.ID())
	if !ok {
		return nil, fmt.Errorf("advisor: unknown thread %s", thread.ID())
	}
	continueSeverity, ok := ParseSeverity(cfg.Advisor.ContinueSeverity)
	if !ok {
		continueSeverity = SeverityBlocker
	}
	r := &Runtime{primary: thread, policy: Policy{ContinueSeverity: continueSeverity, ImmuneTurns: cfg.Advisor.ImmuneTurns}}
	if err := r.addAdvisors(ctx, host, info, cfg); err != nil {
		return nil, errors.Join(err, r.closeChildren())
	}
	life, stop := context.WithCancelCause(context.WithoutCancel(ctx))
	r.stop, r.done = stop, life.Done()
	if err := thread.SetBoundaryHook(r.boundary); err != nil {
		stop(errRuntimeClosed)
		return nil, errors.Join(fmt.Errorf("advisor: install boundary hook: %w", err), r.closeChildren())
	}
	for _, a := range r.advisors {
		r.workers.Go(func() { a.run(life) })
	}
	return r, nil
}

func (r *Runtime) addAdvisors(ctx context.Context, host *agent.Host, info agent.ThreadInfo, cfg config.Config) error {
	on := cfg.Advisor.Enabled && (info.Parent == "" || cfg.Advisor.Subagents)
	for _, name := range slices.Sorted(maps.Keys(cfg.Advisors)) {
		a, err := r.newAdvisor(ctx, host, info, cfg, name)
		if err != nil {
			return err
		}
		a.on = on
		r.advisors = append(r.advisors, a)
	}
	for _, a := range r.advisors {
		err := a.attach(ctx, info)
		close(a.ready)
		if err != nil {
			return err
		}
	}
	return nil
}

func threadInfo(host *agent.Host, id string) (agent.ThreadInfo, bool) {
	for _, info := range host.Threads() {
		if info.ID == id {
			return info, true
		}
	}
	return agent.ThreadInfo{}, false
}

func (r *Runtime) newAdvisor(ctx context.Context, host *agent.Host, info agent.ThreadInfo, cfg config.Config, name string) (*advisor, error) {
	spec := cfg.Advisors[name]
	role := cmp.Or(spec.Model, cfg.Roles.Configured("advisor"), "advisor")
	resolved, err := host.ResolveRole(ctx, role, info.Model)
	if err != nil {
		return nil, fmt.Errorf("advisor: resolve model for %s: %w", name, err)
	}
	a := &advisor{
		rt: r, host: host, name: name, spec: spec, model: resolved, maxNotes: cfg.Advisor.MaxNotes,
		guard: NewGuard(cfg.Advisor.MaxNotes), ready: make(chan struct{}), wake: make(chan struct{}, 1),
		changed: make(chan struct{}),
	}
	tools := []tool.Tool{adviseTool{a: a}}
	for _, granted := range spec.Tools {
		if build, ok := investigative[granted]; ok {
			tools = append(tools, build())
		}
	}
	a.overrides = agent.Overrides{
		Tools:  tools,
		System: System(cfg.Advisor.MaxNotes, spec.Instructions, Watchdog(cfg.Home, info.Cwd)),
	}
	return a, nil
}

func (r *Runtime) boundary(b agent.Boundary) agent.Wait {
	r.mu.Lock()
	if r.steered {
		r.sinceSteer++
	}
	r.mu.Unlock()
	var waits []agent.Wait
	for _, a := range r.advisors {
		if wait := a.boundary(b); wait != nil {
			waits = append(waits, wait)
		}
	}
	if len(waits) == 0 {
		return nil
	}
	return func(ctx context.Context) error {
		for _, wait := range waits {
			if err := wait(ctx); err != nil {
				return err
			}
		}
		return nil
	}
}

func phaseOf(status agent.Status) Phase {
	switch {
	case status.Busy:
		return PhaseRunning
	case status.AutoContinue:
		return PhaseIdle
	default:
		return PhaseInterrupted
	}
}

func (r *Runtime) route(severity Severity, continued bool) (agent.Delivery, error) {
	if continued {
		return agent.DeliveryAside, nil
	}
	status, err := r.primary.Status()
	if err != nil {
		return "", fmt.Errorf("advisor: primary status: %w", err)
	}
	r.mu.Lock()
	state := PrimaryState{Phase: phaseOf(status), Steered: r.steered, TurnsSinceSteer: r.sinceSteer}
	r.mu.Unlock()
	return agent.Delivery(Route(state, severity, r.policy)), nil
}

func (r *Runtime) delivered(delivery agent.Delivery, severity Severity) {
	if delivery != agent.DeliverySteer || severity < SeverityConcern {
		return
	}
	r.mu.Lock()
	r.steered, r.sinceSteer = true, 0
	r.mu.Unlock()
}

func (r *Runtime) Status() ([]Status, error) {
	read, err := r.primary.ReadAfter(0)
	if err != nil {
		return nil, fmt.Errorf("advisor: read primary: %w", err)
	}
	out := make([]Status, 0, len(r.advisors))
	for _, a := range r.advisors {
		status, err := a.status(read.Events)
		if err != nil {
			return nil, err
		}
		out = append(out, status)
	}
	return out, nil
}

func (r *Runtime) SetEnabled(enabled bool) error {
	status, err := r.primary.Status()
	if err != nil {
		return fmt.Errorf("advisor: primary status: %w", err)
	}
	for _, a := range r.advisors {
		a.setEnabled(enabled, status.Head)
	}
	return nil
}

func (r *Runtime) Reset() {
	for _, a := range r.advisors {
		a.reset()
	}
}

func (r *Runtime) Close() error {
	r.stop(errRuntimeClosed)
	err := r.primary.SetBoundaryHook(nil)
	r.workers.Wait()
	if errors.Is(err, agent.ErrClosed) {
		err = nil
	}
	return errors.Join(err, r.closeChildren())
}

func (r *Runtime) closeChildren() error {
	var errs []error
	for _, a := range r.advisors {
		a.mu.Lock()
		child := a.child
		a.mu.Unlock()
		if child != nil {
			errs = append(errs, child.Close())
		}
	}
	return errors.Join(errs...)
}
