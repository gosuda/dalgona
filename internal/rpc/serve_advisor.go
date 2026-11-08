package rpc

import (
	"context"
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/advisor"
)

func advisorUnavailable() *RPCError {
	return &RPCError{Code: codeNotAvailable, Message: "advisor runtime is not available"}
}

func (s *server) ensureAdvisor(ctx context.Context, thread *agent.Thread) (*advisor.Runtime, error) {
	runtime, err := s.opts.Advisors.Attach(ctx, s.host, thread)
	if err != nil {
		return nil, invalidRequest(err.Error())
	}
	return runtime, nil
}

func (s *server) autoAdvisor(ctx context.Context, thread *agent.Thread) {
	if s.opts.Advisors == nil || !s.host.Config().Advisor.Enabled {
		return
	}
	if _, err := s.ensureAdvisor(ctx, thread); err != nil {
		s.log.Warn("rpc: attach advisor runtime", "thread", thread.ID(), "error", err)
	}
}

func (s *server) advisorFor(ctx context.Context, id string) (*advisor.Runtime, error) {
	if s.opts.Advisors == nil {
		return nil, advisorUnavailable()
	}
	if runtime := s.opts.Advisors.Get(id); runtime != nil {
		return runtime, nil
	}
	if len(s.host.Config().Advisors) == 0 {
		return nil, advisorUnavailable()
	}
	thread, err := s.open(ctx, id)
	if err != nil {
		return nil, err
	}
	return s.ensureAdvisor(ctx, thread)
}

func (s *server) advisorStatus(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, threadIDMember(&id)); err != nil {
		return nil, err
	}
	runtime, err := s.advisorFor(ctx, id)
	if err != nil {
		return nil, err
	}
	states, err := runtime.Status()
	if err != nil {
		return nil, invalidRequest(err.Error())
	}
	list := make(advisorList, 0, len(states))
	for _, state := range states {
		list = append(list, AdvisorState{
			Name: state.Name, Model: state.Model, Status: string(state.State), Notes: state.Notes,
			Usage: AdvisorUsage{InputTokens: state.Usage.Input, OutputTokens: state.Usage.Output},
		})
	}
	return encode(list)
}

func (s *server) advisorSet(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	var enabled bool
	err := decodeParams(params, func(name string, d *jsontext.Decoder) error {
		if name == "enabled" {
			v, err := readBool(d)
			enabled = v
			return err
		}
		return threadIDMember(&id)(name, d)
	})
	if err != nil {
		return nil, err
	}
	runtime, err := s.advisorFor(ctx, id)
	if err != nil {
		return nil, err
	}
	if err := runtime.SetEnabled(enabled); err != nil {
		return nil, invalidRequest(err.Error())
	}
	return emptyObject(), nil
}

func (s *server) advisorReset(ctx context.Context, params jsontext.Value) (jsontext.Value, error) {
	var id string
	if err := decodeParams(params, threadIDMember(&id)); err != nil {
		return nil, err
	}
	runtime, err := s.advisorFor(ctx, id)
	if err != nil {
		return nil, err
	}
	runtime.Reset()
	return emptyObject(), nil
}
