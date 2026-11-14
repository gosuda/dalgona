package ask

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/model"
)

type invalid string

func (e invalid) Error() string { return string(e) }

type rawOption struct {
	label, description, preview string
}

type rawQuestion struct {
	header, text string
	options      []rawOption
	hasOptions   bool
	multiSelect  bool
}

func Parse(raw jsontext.Value) ([]Question, error) {
	questions, err := readArguments(raw)
	if err != nil {
		if bad, ok := errors.AsType[invalid](err); ok {
			return nil, bad
		}
		return nil, fmt.Errorf("ask: arguments do not match the schema: %w", err)
	}
	return validate(questions)
}

func unknownField(name string) error {
	return invalid(fmt.Sprintf("ask: unknown field %q", name))
}

func rejectNull(decoder *jsontext.Decoder, where string) error {
	if decoder.PeekKind() == 'n' {
		return fmt.Errorf("%s must not be null", where)
	}
	return nil
}

func readArguments(raw jsontext.Value) ([]rawQuestion, error) {
	var questions []rawQuestion
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	if err := rejectNull(decoder, "arguments"); err != nil {
		return nil, err
	}
	err := model.ReadObjectFields(decoder, func(name string) error {
		if name != "questions" {
			return unknownField(name)
		}
		var err error
		questions, err = readQuestions(decoder)
		return err
	})
	if err != nil {
		return nil, err
	}
	if _, extra := decoder.ReadValue(); !errors.Is(extra, io.EOF) {
		return nil, errors.New("unexpected data after the arguments object")
	}
	return questions, nil
}

func readList[T any](decoder *jsontext.Decoder, name string, readItem func(*jsontext.Decoder, *T) error) ([]T, error) {
	if err := rejectNull(decoder, name); err != nil {
		return nil, err
	}
	var items []T
	err := model.ReadArrayItems(decoder, func(int) error {
		var item T
		err := readItem(decoder, &item)
		items = append(items, item)
		return err
	})
	return items, err
}

func readQuestions(decoder *jsontext.Decoder) ([]rawQuestion, error) {
	return readList(decoder, "questions", readQuestion)
}

func readQuestion(decoder *jsontext.Decoder, question *rawQuestion) error {
	if err := rejectNull(decoder, "question"); err != nil {
		return err
	}
	texts := map[string]*string{"header": &question.header, "question": &question.text}
	return model.ReadObjectFields(decoder, func(name string) error {
		switch name {
		case "options":
			question.hasOptions = true
			var err error
			question.options, err = readOptions(decoder)
			return err
		case "multi_select":
			return readFlag(decoder, &question.multiSelect)
		}
		target, ok := texts[name]
		if !ok {
			return unknownField(name)
		}
		return readText(decoder, name, target)
	})
}

func readOptions(decoder *jsontext.Decoder) ([]rawOption, error) {
	return readList(decoder, "options", readOption)
}

func readOption(decoder *jsontext.Decoder, option *rawOption) error {
	if err := rejectNull(decoder, "option"); err != nil {
		return err
	}
	texts := map[string]*string{"label": &option.label, "description": &option.description, "preview": &option.preview}
	return model.ReadObjectFields(decoder, func(name string) error {
		target, ok := texts[name]
		if !ok {
			return unknownField(name)
		}
		return readText(decoder, name, target)
	})
}

func readText(decoder *jsontext.Decoder, name string, target *string) error {
	if err := rejectNull(decoder, name); err != nil {
		return err
	}
	value, err := model.ReadString(decoder)
	*target = value
	return err
}

func readFlag(decoder *jsontext.Decoder, target *bool) error {
	if err := rejectNull(decoder, "multi_select"); err != nil {
		return err
	}
	var err error
	*target, err = model.ReadBool(decoder)
	return err
}

func validate(raw []rawQuestion) ([]Question, error) {
	if len(raw) < 1 || len(raw) > MaxQuestions {
		return nil, invalid(fmt.Sprintf("ask: questions needs 1 to %d items, got %d", MaxQuestions, len(raw)))
	}
	questions := make([]Question, 0, len(raw))
	for i, item := range raw {
		question, err := validateQuestion(i+1, item)
		if err != nil {
			return nil, err
		}
		for j, earlier := range questions {
			if strings.EqualFold(earlier.Text, question.Text) {
				return nil, invalid(fmt.Sprintf("ask: question %d repeats question %d", i+1, j+1))
			}
		}
		questions = append(questions, question)
	}
	return questions, nil
}

func validateQuestion(index int, raw rawQuestion) (Question, error) {
	prefix := fmt.Sprintf("ask: question %d", index)
	header := CleanLine(raw.header)
	text := CleanLine(raw.text)
	if err := checkLength(prefix, "header", header, MaxHeader); err != nil {
		return Question{}, err
	}
	if err := checkLength(prefix, "question", text, MaxQuestion); err != nil {
		return Question{}, err
	}
	if raw.hasOptions && (len(raw.options) < MinOptions || len(raw.options) > MaxOptions) {
		return Question{}, invalid(fmt.Sprintf("%s: options needs %d to %d items, or none for a free-text question, got %d", prefix, MinOptions, MaxOptions, len(raw.options)))
	}
	if raw.multiSelect && len(raw.options) == 0 {
		return Question{}, invalid(prefix + ": multi_select needs options")
	}
	options, err := validateOptions(index, raw.options)
	if err != nil {
		return Question{}, err
	}
	return Question{Header: header, Text: text, Options: options, MultiSelect: raw.multiSelect}, nil
}

func validateOptions(index int, raw []rawOption) ([]Option, error) {
	var options []Option
	for j, item := range raw {
		option, err := validateOption(index, j+1, item)
		if err != nil {
			return nil, err
		}
		options = append(options, option)
		if k := firstRepeat(options, j); k >= 0 {
			return nil, invalid(fmt.Sprintf("ask: question %d option %d: label %q repeats option %d", index, j+1, option.Label, k+1))
		}
		if strings.EqualFold(option.Label, "other") {
			return nil, invalid(fmt.Sprintf("ask: question %d option %d: the user always gets an Other row; remove this option", index, j+1))
		}
	}
	return options, nil
}

func firstRepeat(options []Option, j int) int {
	for k := range j {
		if strings.EqualFold(options[k].Label, options[j].Label) {
			return k
		}
	}
	return -1
}

func validateOption(index, j int, raw rawOption) (Option, error) {
	where := fmt.Sprintf("ask: question %d option %d", index, j)
	label := CleanLine(raw.label)
	if err := checkLength(where, "label", label, MaxLabel); err != nil {
		return Option{}, err
	}
	description := CleanLine(raw.description)
	if err := checkLength(where, "description", description, MaxDescription); err != nil {
		return Option{}, err
	}
	preview := CleanBlock(raw.preview)
	if utf8.RuneCountInString(preview) > MaxPreview || lineCount(preview) > MaxPreviewLines {
		return Option{}, invalid(fmt.Sprintf("%s: preview is longer than %d characters or %d lines", where, MaxPreview, MaxPreviewLines))
	}
	return Option{Label: label, Description: description, Preview: preview}, nil
}

func checkLength(prefix, field, value string, limit int) error {
	if value == "" {
		return invalid(prefix + ": " + field + " is empty")
	}
	if utf8.RuneCountInString(value) > limit {
		return invalid(fmt.Sprintf("%s: %s is longer than %d characters", prefix, field, limit))
	}
	return nil
}

func lineCount(text string) int {
	if text == "" {
		return 0
	}
	return strings.Count(text, "\n") + 1
}
