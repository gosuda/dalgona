package decide

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"strings"
	"time"
)

const askTimeout = 8 * time.Second

const (
	AdapterDecisions = "decisions"
	AdapterLLM       = "llm"
)

type Result struct {
	Answers  map[string]Answer
	Model    string
	Fallback string
}

type Client interface {
	Ask(context.Context, jsontext.Value, map[string]Question) (Result, error)
}

type QuestionType string

const (
	TypeChoice QuestionType = "choice"
	TypeNoul   QuestionType = "noul"
	TypeScore  QuestionType = "score"
)

type Question struct {
	Type         QuestionType
	Instructions string
	Criteria     any
}

type Answer struct {
	Choice        string
	Noul          *float64
	Score         *float64
	Probabilities map[string]float64
	Confidence    float64
}

type Spec struct {
	Adapter string
	Model   string
}

func Parse(value string) (Spec, error) {
	adapter, modelID, ok := strings.Cut(value, ":")
	if !ok || modelID == "" || (adapter != AdapterDecisions && adapter != AdapterLLM) {
		return Spec{}, errors.New("decide: spec must be decisions:<id> or llm:<role or id>")
	}
	return Spec{Adapter: adapter, Model: modelID}, nil
}

var (
	ErrAnswer  = errors.New("decide: invalid answer")
	ErrRequest = errors.New("decide: invalid request")
)

type categoryError struct {
	cause    error
	category error
}

func (e *categoryError) Error() string {
	return e.cause.Error()
}

func (e *categoryError) Unwrap() []error {
	return []error{e.category, e.cause}
}

func errAnswer(cause error) error {
	return &categoryError{cause: cause, category: ErrAnswer}
}

func errRequest(cause error) error {
	return &categoryError{cause: cause, category: ErrRequest}
}
