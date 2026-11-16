package decide

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/dal/model"
)

type Named struct {
	Client Client
	Model  string
}

type Cooldowns struct {
	mu    sync.Mutex
	until map[string]time.Time
	now   func() time.Time
}

type CooldownError struct {
	Model string
	Until time.Time
}

type FallbackError struct {
	Primary    string
	PrimaryErr error
	Smol       string
	SmolErr    error
}

func NewCooldowns(now func() time.Time) *Cooldowns {
	if now == nil {
		now = time.Now
	}
	return &Cooldowns{until: make(map[string]time.Time), now: now}
}

func (e *CooldownError) Error() string {
	return "cooling down until " + e.Until.UTC().Format(time.RFC3339Nano)
}

func (e *FallbackError) Error() string {
	return fmt.Sprintf("%v; fallback %s: %v", e.PrimaryErr, e.Smol, e.SmolErr)
}

func (e *FallbackError) Unwrap() []error {
	return []error{e.PrimaryErr, e.SmolErr}
}

func (c *Cooldowns) active(key string) (time.Time, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	until, ok := c.until[key]
	if !ok {
		return time.Time{}, false
	}
	if !until.After(c.now()) {
		delete(c.until, key)
		return time.Time{}, false
	}
	return until, true
}

func (c *Cooldowns) set(key string, until time.Time) {
	c.mu.Lock()
	defer c.mu.Unlock()
	existing, ok := c.until[key]
	if ok && existing.After(until) {
		return
	}
	c.until[key] = until
}

func WithFallback(primary, smol Named, cooldowns *Cooldowns) Client {
	if smol.Client == nil {
		return primary.Client
	}
	if cooldowns == nil {
		cooldowns = NewCooldowns(nil)
	}
	if sameModelIdentity(primary.Model, smol.Model) {
		return primary.Client
	}
	return &fallbackClient{primary: primary, smol: smol, cooldowns: cooldowns}
}

func sameModelIdentity(primary, smol string) bool {
	return primary != "" && primary == smol
}

type fallbackClient struct {
	primary   Named
	smol      Named
	cooldowns *Cooldowns
}

func (f *fallbackClient) Ask(ctx context.Context, state jsontext.Value, questions map[string]Question) (Result, error) {
	if err := context.Cause(ctx); err != nil {
		return Result{}, err
	}
	primaryResult, primaryErr := f.primaryAttempt(ctx, state, questions)
	if primaryErr == nil {
		if err := context.Cause(ctx); err != nil {
			return Result{}, err
		}
		return primaryResult, nil
	}
	if err := context.Cause(ctx); err != nil {
		return Result{}, err
	}
	return f.smolAttempt(ctx, state, questions, primaryErr)
}

func (f *fallbackClient) primaryAttempt(ctx context.Context, state jsontext.Value, questions map[string]Question) (Result, error) {
	if until, ok := f.cooldowns.active(f.primary.Model); ok {
		return Result{}, &CooldownError{Model: f.primary.Model, Until: until}
	}
	result, err := f.attempt(ctx, f.primary.Client, state, questions)
	if err != nil {
		if context.Cause(ctx) == nil {
			f.recordCooldown(err)
		}
		return Result{}, err
	}
	return result, nil
}

func (f *fallbackClient) smolAttempt(ctx context.Context, state jsontext.Value, questions map[string]Question, primaryErr error) (Result, error) {
	result, err := f.attempt(ctx, f.smol.Client, state, questions)
	if err == nil {
		if cause := context.Cause(ctx); cause != nil {
			err = cause
		}
	}
	if err != nil {
		return Result{}, &FallbackError{
			Primary:    f.primary.Model,
			PrimaryErr: primaryErr,
			Smol:       f.smol.Model,
			SmolErr:    err,
		}
	}
	return Result{Answers: result.Answers, Model: result.Model, Fallback: f.primary.Model + ": " + primaryErr.Error()}, nil
}

func (f *fallbackClient) attempt(ctx context.Context, client Client, state jsontext.Value, questions map[string]Question) (Result, error) {
	attemptCtx, cancel := context.WithTimeout(ctx, askTimeout)
	result, err := client.Ask(attemptCtx, state, questions)
	attemptCause := context.Cause(attemptCtx)
	cancel()
	if err == nil {
		err = validResult(result, questions)
	}
	if err != nil {
		if context.Cause(ctx) == nil && attemptCause != nil && !typedHTTPError(err) {
			err = attemptCause
		}
		return Result{}, err
	}
	return result, nil
}

func typedHTTPError(err error) bool {
	if _, ok := errors.AsType[*model.HTTPError](err); ok {
		return true
	}
	_, ok := errors.AsType[*HTTPError](err)
	return ok
}

func validResult(result Result, questions map[string]Question) error {
	if result.Model == "" {
		return errAnswer(errors.New("decide: empty answering model"))
	}
	if len(result.Answers) != len(questions) {
		return errAnswer(errors.New("decide: missing answers"))
	}
	return nil
}

func (f *fallbackClient) recordCooldown(err error) {
	if errors.Is(err, ErrAnswer) || errors.Is(err, ErrRequest) {
		return
	}
	wait := retryAfterOf(err)
	if wait < cooldownFloor {
		wait = cooldownFloor
	}
	f.cooldowns.set(f.primary.Model, f.cooldowns.now().Add(wait))
}

func retryAfterOf(err error) time.Duration {
	if typed, ok := errors.AsType[*model.HTTPError](err); ok && typed.RetryAfter > 0 {
		return typed.RetryAfter
	}
	if typed, ok := errors.AsType[*HTTPError](err); ok && typed.RetryAfter > 0 {
		return typed.RetryAfter
	}
	return 0
}

const cooldownFloor = 60 * time.Second
