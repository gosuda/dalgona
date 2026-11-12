package ask

import (
	"encoding/json/jsontext"
	"fmt"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/tool"
)

const (
	TextNoClient       = "declined: no client could answer this question"
	TextSkipped        = "The user skipped the questions and has no preference. Choose for yourself and say which choice you made."
	TextCancelled      = "The user dismissed the questions without answering. Continue with your best judgment, or stop and say what you need if you cannot continue."
	TextInterrupted    = "interrupted: the user interrupted the turn before answering"
	TextRestarted      = "interrupted: the host restarted before the user answered"
	TextUnknownOutcome = "ask: unknown reply outcome"
	answersHead        = "User answers:"
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
	for _, text := range raw {
		value := cutRunes(CleanLine(text), MaxAnswer)
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

func cutRunes(text string, limit int) string {
	runes := []rune(text)
	if len(runes) <= limit {
		return text
	}
	return string(runes[:limit])
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
		lines = append(lines, fmt.Sprintf("- %s (%s): %s", question.Header, question.Text, describe(question, answers[i])))
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

func formatPick(question Question, pick string) string {
	if question.hasLabel(pick) {
		return pick
	}
	quoted := quote(pick)
	if len(question.Options) == 0 {
		return quoted
	}
	return "Other: " + quoted
}

func quote(text string) string {
	quoted, _ := jsontext.AppendQuote(nil, text)
	return string(quoted)
}
