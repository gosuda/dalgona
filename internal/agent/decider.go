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
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
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
	return clientDecider{client: client, model: modelName, threshold: threshold, product: product}
}

type ClientFactory func(ctx context.Context, threadID string) (decide.Client, string, error)

type factoryDecider struct {
	factory   ClientFactory
	threshold float64
	product   config.Product
}

func NewFactoryDecider(factory ClientFactory, threshold float64, product config.Product) Decider {
	return factoryDecider{factory: factory, threshold: threshold, product: product}
}

func (d factoryDecider) Decide(ctx context.Context, a Action) (Decision, error) {
	client, name, err := d.factory(ctx, a.Thread)
	if err != nil {
		return Decision{}, err
	}
	return NewDecider(client, name, d.threshold, d.product).Decide(ctx, a)
}

func (d clientDecider) Decide(ctx context.Context, a Action) (Decision, error) {
	state, err := d.state(a)
	if err != nil {
		return Decision{}, err
	}
	questions := deciderQuestions(a)
	result, err := d.ask(ctx, state, questions)
	if err != nil {
		return Decision{By: "decider:" + d.model, Reason: fmt.Sprintf("declined by decider %s: %v", d.model, err)}, nil
	}
	scores, err := scoreAnswers(result.Answers, questions)
	if err != nil {
		return Decision{By: "decider:" + result.Model, Reason: fmt.Sprintf("declined by decider %s: %v", result.Model, err)}, nil
	}
	allow := d.allows(scores, questions)
	verb := "declined"
	if allow {
		verb = "allowed"
	}
	reason := fmt.Sprintf("%s by decider %s: %s (threshold %s)", verb, askAttribution(result), scoreText(scores), formatScore(d.threshold))
	return Decision{Allow: allow, By: "decider:" + result.Model, Reason: reason, Scores: scores}, nil
}

func deciderQuestions(a Action) map[string]decide.Question {
	questions := map[string]decide.Question{"reversible": {Type: decide.TypeNoul, Instructions: questionReversible}}
	if a.Task != "" {
		questions["serves_task"] = decide.Question{Type: decide.TypeNoul, Instructions: questionServesTask}
	}
	return questions
}

func (d clientDecider) ask(ctx context.Context, state jsontext.Value, questions map[string]decide.Question) (decide.Result, error) {
	if d.timeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeoutCause(ctx, d.timeout, fmt.Errorf("decider timed out after %s", d.timeout))
		defer cancel()
	}
	result, err := d.client.Ask(ctx, state, questions)
	if err != nil && d.timeout > 0 {
		if cause := context.Cause(ctx); cause != nil {
			err = cause
		}
	}
	return result, err
}

func (d clientDecider) allows(scores map[string]float64, questions map[string]decide.Question) bool {
	allow := scores["reversible"] >= d.threshold
	if _, asked := questions["serves_task"]; asked {
		allow = allow && scores["serves_task"] >= d.threshold
	}
	return allow
}

func askAttribution(result decide.Result) string {
	if result.Fallback != "" {
		return result.Model + " (fallback from " + result.Fallback + ")"
	}
	return result.Model
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
