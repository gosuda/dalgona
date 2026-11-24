package ask

import (
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/textinput"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

const (
	TextNoClient         = "declined: no client could answer this question"
	TextSkipped          = "The user skipped the questions and has no preference. Choose for yourself and say which choice you made."
	TextCancelled        = "The user dismissed the questions without answering. Continue with your best judgment, or stop and say what you need if you cannot continue."
	TextInterrupted      = "interrupted: the user interrupted the turn before answering"
	TextRestarted        = "interrupted: the host restarted before the user answered"
	TextTimedOut         = "No answer before the timeout. The recommended options were used instead of the user's answers:"
	TextTimedOutNoAnswer = "No answer before the timeout. The user has not chosen. Choose for yourself and say which choice you made."
	TextUnknownOutcome   = "ask: unknown reply outcome"
	answersHead          = "User answers:"
)

func Answers(questions []Question, byID map[string][]string) [][]string {
	answers := make([][]string, len(questions))
	for i, question := range questions {
		answers[i] = answerFor(question, byID[QuestionID(i)])
	}
	return answers
}

func answerFor(question Question, raw []string) []string {
	picked := map[string]bool{}
	seen := map[string]bool{}
	var typed []string
	for _, value := range raw {
		if value == "" || seen[value] {
			continue
		}
		seen[value] = true
		if question.hasLabel(value) {
			picked[value] = true
		} else {
			typed = append(typed, value)
		}
	}
	var answer []string
	for _, option := range question.Options {
		if picked[option.Label] {
			answer = append(answer, option.Label)
		}
	}
	answer = append(answer, typed...)
	limit := 1
	if question.MultiSelect {
		limit = len(question.Options) + 1
	}
	return answer[:min(len(answer), limit)]
}

func CheckAnswers(byID map[string][]string) *textinput.Error {
	for _, id := range slices.Sorted(maps.Keys(byID)) {
		for i, text := range byID[id] {
			field := textinput.Field{Name: "answers." + id + ".answers[" + strconv.Itoa(i) + "]", MaxScalars: MaxAnswer}
			if textErr, ok := errors.AsType[*textinput.Error](field.Check(text)); ok {
				return textErr
			}
		}
	}
	return nil
}

func Format(questions []Question, reply Reply) tool.Result {
	switch reply.Outcome {
	case OutcomeAnswered:
		return answered(questions, reply.Answers)
	case OutcomeCancelled:
		return tool.Result{Output: []byte(TextCancelled), OK: true}
	case OutcomeNoClient:
		return tool.Result{Output: []byte(TextNoClient)}
	case OutcomeInterrupted:
		return tool.Result{Output: []byte(TextInterrupted)}
	case OutcomeFailed:
		return declined(reply.Detail)
	case OutcomeTimedOut:
		return timedOut(questions)
	default:
		return tool.Result{Output: []byte(TextUnknownOutcome)}
	}
}

func answered(questions []Question, replies [][]string) tool.Result {
	byID := make(map[string][]string, len(replies))
	for i, picks := range replies {
		byID[QuestionID(i)] = picks
	}
	answers := Answers(questions, byID)
	if !slices.ContainsFunc(answers, func(picks []string) bool { return len(picks) > 0 }) {
		return tool.Result{Output: []byte(TextSkipped), OK: true}
	}
	return tool.Result{Output: []byte(answeredText(questions, answers)), OK: true}
}

func declined(detail string) tool.Result {
	return tool.Result{Output: []byte("declined: " + detail)}
}

func answeredText(questions []Question, answers [][]string) string {
	lines := []string{answersHead}
	for i, question := range questions {
		lines = append(lines, questionLine(question, answers[i]))
	}
	return strings.Join(lines, "\n")
}

func describe(question Question, picks []string) string {
	if len(picks) == 0 {
		return "no answer"
	}
	parts := make([]string, 0, len(picks))
	for _, pick := range picks {
		parts = append(parts, formatPick(question, pick))
	}
	return strings.Join(parts, "; ")
}

func questionLine(question Question, picks []string) string {
	return fmt.Sprintf("- %s (%s): %s", textinput.DisplayLine(question.Header), textinput.DisplayLine(question.Text), describe(question, picks))
}

func formatPick(question Question, pick string) string {
	labels := make([]string, len(question.Options))
	for i, option := range question.Options {
		labels[i] = option.Label
	}
	if i := slices.Index(labels, pick); i >= 0 {
		return textinput.DistinctLines(labels)[i]
	}
	quoted := textinput.Quote(pick)
	if len(question.Options) == 0 {
		return quoted
	}
	return "Other: " + quoted
}

func timedOut(questions []Question) tool.Result {
	if !slices.ContainsFunc(questions, func(q Question) bool { return len(q.Options) > 0 }) {
		return tool.Result{Output: []byte(TextTimedOutNoAnswer), OK: true}
	}
	defaults := recommended(questions)
	lines := make([]string, 0, len(questions)+1)
	lines = append(lines, TextTimedOut)
	for i, question := range questions {
		lines = append(lines, questionLine(question, defaults[i]))
	}
	return tool.Result{Output: []byte(strings.Join(lines, "\n")), OK: true}
}

func recommended(questions []Question) [][]string {
	defaults := make([][]string, len(questions))
	for i, question := range questions {
		if len(question.Options) > 0 {
			defaults[i] = []string{question.Options[0].Label}
		}
	}
	return defaults
}
