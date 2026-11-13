package rpc

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"

	"github.com/gosuda/dalgona/internal/agent"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const methodRequestUserInput = "item/tool/requestUserInput"

type questionAsk struct {
	thread    *agent.Thread
	window    <-chan agent.Update
	requestID string
	questions []asktool.Question
	params    ToolRequestUserInputParams
}

func (s *server) question(ctx context.Context, w *watch, ch <-chan agent.Update, update agent.Update) {
	prompt := update.Question
	if prompt.Resolved {
		s.questionResolved(w, prompt)
		return
	}
	w.tracker.setStatus("active", "waitingOnUserInput")
	q := questionAsk{
		thread:    w.thread,
		window:    ch,
		requestID: prompt.RequestID,
		questions: prompt.Questions,
		params:    userInputParams(update.Thread, w.tracker.currentTurn(), prompt),
	}
	askCtx, cancel := context.WithCancel(ctx)
	started, dup := s.spawnRequest(prompt.RequestID, cancel, func() {
		defer s.settle(prompt.RequestID)
		s.requestInput(ctx, askCtx, q)
	})
	if dup || !started {
		cancel()
	}
}

func (s *server) questionResolved(w *watch, prompt agent.QuestionPrompt) {
	s.settle(prompt.RequestID)
	if w.tracker.turn != "" {
		w.tracker.setStatus("active")
	}
	s.send("serverRequest/resolved", ServerRequestResolvedNotification{
		ThreadID: w.thread.ID(), RequestID: prompt.RequestID, Resolution: resolutionName(prompt.Resolution),
	})
}

func (s *server) requestInput(serveCtx, askCtx context.Context, q questionAsk) {
	params, err := encode(q.params)
	if err != nil {
		s.log.Warn("rpc: encode question", "request", q.requestID, "error", err)
		s.answerQuestion(q, asktool.Reply{Outcome: asktool.OutcomeFailed, Detail: "the question could not be encoded: " + err.Error()})
		return
	}
	var response jsontext.Value
	err = s.conn.CallID(askCtx, q.requestID, methodRequestUserInput, params, &response)
	switch {
	case err == nil:
		s.answerQuestion(q, inputReply(q.questions, response))
	case askCtx.Err() != nil, serveCtx.Err() != nil, errors.Is(err, io.ErrClosedPipe):
	default:
		s.cannotAnswer(q, err)
	}
}

func (s *server) cannotAnswer(q questionAsk, cause error) {
	s.log.Info("rpc: question request failed; this window cannot answer", "request", q.requestID, "error", cause)
	if err := q.thread.CannotAnswerQuestion(q.requestID, q.window); err != nil && !errors.Is(err, agent.ErrUnknownRequest) {
		s.log.Warn("rpc: report unanswerable question", "request", q.requestID, "error", err)
	}
}

func (s *server) answerQuestion(q questionAsk, reply asktool.Reply) {
	err := q.thread.AnswerQuestion(q.requestID, reply)
	if err != nil && !errors.Is(err, agent.ErrUnknownRequest) {
		s.log.Warn("rpc: answer question", "request", q.requestID, "error", err)
	}
}

func inputReply(questions []asktool.Question, response jsontext.Value) asktool.Reply {
	byID, err := answersByID(response)
	if err != nil {
		return asktool.Reply{Outcome: asktool.OutcomeFailed, Detail: "the client sent an unreadable answer: " + err.Error()}
	}
	if len(byID) == 0 {
		return asktool.Reply{Outcome: asktool.OutcomeCancelled}
	}
	return asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: asktool.Answers(questions, byID)}
}

func answersByID(response jsontext.Value) (map[string][]string, error) {
	byID := map[string][]string{}
	found := false
	err := decodeParams(response, func(name string, d *jsontext.Decoder) error {
		if name != "answers" {
			return d.SkipValue()
		}
		found = true
		return readMembers(d, func(id string, d *jsontext.Decoder) error {
			return readAnswerList(d, id, byID)
		})
	})
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, errors.New("answers is missing")
	}
	return byID, nil
}

func readAnswerList(d *jsontext.Decoder, id string, byID map[string][]string) error {
	var picks []string
	err := readMembers(d, func(name string, d *jsontext.Decoder) error {
		if name != "answers" {
			return d.SkipValue()
		}
		return readArray(d, func() error {
			text, err := readString(d)
			if err != nil {
				return fmt.Errorf("answer %s: %w", id, err)
			}
			picks = append(picks, text)
			return nil
		})
	})
	byID[id] = picks
	return err
}

func resolutionName(outcome asktool.Outcome) string {
	switch outcome {
	case asktool.OutcomeAnswered:
		return "answered"
	case asktool.OutcomeCancelled:
		return "cancelled"
	case asktool.OutcomeInterrupted:
		return "interrupted"
	case asktool.OutcomeNoClient, asktool.OutcomeFailed:
	}
	return "declined"
}

func userInputParams(threadID, turnID string, prompt agent.QuestionPrompt) ToolRequestUserInputParams {
	questions := make([]ToolRequestUserInputQuestion, len(prompt.Questions))
	for i, question := range prompt.Questions {
		item := ToolRequestUserInputQuestion{
			ID: asktool.QuestionID(i), Header: question.Header, Question: question.Text,
			IsOther: len(question.Options) > 0 && !question.NoOther, MultiSelect: question.MultiSelect,
		}
		for _, option := range question.Options {
			item.Options = append(item.Options, ToolRequestUserInputOption{
				Label: option.Label, Description: option.Description, Preview: option.Preview,
			})
		}
		questions[i] = item
	}
	return ToolRequestUserInputParams{
		ThreadID: threadID, TurnID: turnID, ItemID: prompt.CallID, IsBlocking: true, Questions: questions,
	}
}
