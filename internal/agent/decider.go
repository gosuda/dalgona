package agent

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"fmt"
	"strconv"
	"time"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	deciderTimeout     = 8 * time.Second
	questionReversible = "Every command in `commands` only reads or changes files inside `project` and can be undone with git or by rerunning it. It does not push, publish, deploy, delete files outside the project, change system settings, or send data to a network service."
	questionServesTask = "Running `commands` is a reasonable next step toward `task`."
)

type Action struct {
	Thread        string
	Tool          string
	Class         tool.Class
	Commands      []string
	Paths         []string
	Text          string
	Project       string
	Task          string
	Justification string
	Sandboxed     bool
}

type Decision struct {
	Allow  bool
	By     string
	Reason string
	Scores map[string]float64
}

type Decider interface {
	Decide(ctx context.Context, a Action) (Decision, error)
}

type clientDecider struct {
	client    decide.Client
	model     string
	threshold float64
	product   config.Product
	timeout   time.Duration
}

func NewDecider(client decide.Client, modelName string, threshold float64, product config.Product) Decider {
	return clientDecider{client: client, model: modelName, threshold: threshold, product: product, timeout: deciderTimeout}
}

func (d clientDecider) Decide(ctx context.Context, a Action) (Decision, error) {
	by := "decider:" + d.model
	state, err := d.state(a)
	if err != nil {
		return Decision{}, err
	}
	questions := map[string]decide.Question{"reversible": {Type: decide.TypeNoul, Instructions: questionReversible}}
	if a.Task != "" {
		questions["serves_task"] = decide.Question{Type: decide.TypeNoul, Instructions: questionServesTask}
	}
	askCtx, cancel := context.WithTimeoutCause(ctx, d.timeout, fmt.Errorf("decider timed out after %s", d.timeout))
	defer cancel()
	answers, err := d.client.Ask(askCtx, state, questions)
	if err != nil {
		if cause := context.Cause(askCtx); cause != nil {
			err = cause
		}
		return Decision{By: by, Reason: fmt.Sprintf("declined by decider %s: %v", d.model, err)}, nil
	}
	scores, err := scoreAnswers(answers, questions)
	if err != nil {
		return Decision{By: by, Reason: fmt.Sprintf("declined by decider %s: %v", d.model, err)}, nil
	}
	allow := scores["reversible"] >= d.threshold
	if _, asked := questions["serves_task"]; asked {
		allow = allow && scores["serves_task"] >= d.threshold
	}
	verb := "declined"
	if allow {
		verb = "allowed"
	}
	reason := fmt.Sprintf("%s by decider %s: %s (threshold %s)", verb, d.model, scoreText(scores), formatScore(d.threshold))
	return Decision{Allow: allow, By: by, Reason: reason, Scores: scores}, nil
}

func scoreAnswers(answers map[string]decide.Answer, questions map[string]decide.Question) (map[string]float64, error) {
	scores := make(map[string]float64, len(questions))
	for name := range questions {
		answer, ok := answers[name]
		value := cmp.Or(answer.Noul, answer.Score)
		if !ok || value == nil || *value < 0 || *value > 1 {
			return nil, fmt.Errorf("answer %s is missing or out of range", name)
		}
		scores[name] = *value
	}
	return scores, nil
}

func scoreText(scores map[string]float64) string {
	text := "reversible=" + formatScore(scores["reversible"])
	if value, ok := scores["serves_task"]; ok {
		text += " serves_task=" + formatScore(value)
	}
	return text
}

func formatScore(value float64) string { return strconv.FormatFloat(value, 'f', -1, 64) }

func (d clientDecider) state(a Action) (jsontext.Value, error) {
	var out bytes.Buffer
	encoder := model.NewJSONEncoder(&out)
	encoder.BeginObject()
	encoder.Key("commands")
	encoder.BeginArray()
	for _, command := range a.Commands {
		encoder.String(command)
	}
	encoder.EndArray()
	encoder.StringKey("project", a.Project)
	encoder.StringKey("task", a.Task)
	encoder.StringKey("agent", string(d.product))
	if a.Justification != "" {
		encoder.StringKey("justification", a.Justification)
	}
	encoder.BoolKey("sandboxed", a.Sandboxed)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("agent: encode decider state: %w", err)
	}
	return jsontext.Value(bytes.TrimSpace(out.Bytes())), nil
}
