//go:build gui

package panel

import (
	"slices"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

var presetOrder = [...]string{"default", "smol", "slow"}

type settingEdit struct {
	KeyPath       string `json:"keyPath"`
	Value         any    `json:"value"`
	MergeStrategy string `json:"mergeStrategy"`
}

type batchWrite struct {
	Edits []settingEdit `json:"edits"`
}

func replaceEdit(path string, value any) settingEdit {
	return settingEdit{KeyPath: path, Value: value, MergeStrategy: "replace"}
}

type presetRow struct {
	name  string
	model rpc.Model
}

type modelPanel struct {
	endpoint client.EndpointID
	picker   endpointPicker
	list     pending
	config   pending
	models   []rpc.Model
	listed   bool
	read     bool
	loadErr  string
	retry    Button
	model    string
	effort   string
	preset   int
	presets  []Choice
	choices  []Choice
	efforts  []Choice
	login    Button
	op       operation
}

func (m *modelPanel) open(r shell.Route) {
	*m = modelPanel{endpoint: r.Thread.Endpoint, preset: -1, picker: m.picker}
}

func (m *modelPanel) release(out *[]state.Intent) {
	m.list.release(out)
	m.config.release(out)
	m.op.call.release(out)
}

func (m *modelPanel) load(s *scene) {
	m.listed, m.read, m.loadErr = false, false, ""
	m.list.start(s, request{endpoint: m.endpoint, method: "model/list", params: rpc.ModelListParams{}})
	m.config.start(s, request{endpoint: m.endpoint, method: "config/read", params: rpc.ConfigReadParams{}})
}

func visibleModels(all []rpc.Model) []rpc.Model {
	return slices.DeleteFunc(slices.Clone(all), func(m rpc.Model) bool { return m.Hidden })
}

func (m *modelPanel) settle(s *scene) {
	if v, done := m.list.settle(s); done {
		list, err := decodeCall[rpc.ModelListResponse](v)
		m.models, m.listed = visibleModels(list.Data), err == nil
		m.fail(err)
	}
	if v, done := m.config.settle(s); done {
		cfg, err := decodeCall[rpc.ConfigReadResponse](v)
		if err == nil {
			m.model, m.effort, m.read = cfg.Config.Model, cfg.Config.ModelReasoningEffort, true
		}
		m.fail(err)
	}
}

func (m *modelPanel) fail(err error) {
	if err != nil && m.loadErr == "" {
		m.loadErr = err.Error()
	}
}

func (m *modelPanel) chosen() (rpc.Model, bool) {
	at := slices.IndexFunc(m.models, func(x rpc.Model) bool { return x.ID == m.model })
	if at < 0 {
		return rpc.Model{}, false
	}
	return m.models[at], true
}

func efforts(model rpc.Model) []string {
	out := make([]string, 0, len(model.SupportedReasoningEfforts))
	for _, e := range model.SupportedReasoningEfforts {
		out = append(out, e.ReasoningEffort)
	}
	return out
}

func fallbackEffort(model rpc.Model) string {
	if model.DefaultReasoningEffort != "" {
		return model.DefaultReasoningEffort
	}
	if len(model.SupportedReasoningEfforts) > 0 {
		return model.SupportedReasoningEfforts[0].ReasoningEffort
	}
	return ""
}

func (m *modelPanel) pick(model rpc.Model) {
	m.model, m.op.err = model.ID, ""
	if !slices.Contains(efforts(model), m.effort) {
		m.effort = fallbackEffort(model)
	}
}

func (m *modelPanel) presetRows() []presetRow {
	var out []presetRow
	for _, name := range presetOrder {
		for _, model := range m.models {
			if slices.Contains(model.Presets, name) {
				out = append(out, presetRow{name: name, model: model})
			}
		}
	}
	return out
}

func (m *modelPanel) choose(gtx layout.Context) {
	presets := m.presetRows()
	for i := range min(len(m.presets), len(presets)) {
		if m.presets[i].Clicked(gtx) {
			m.model, m.effort, m.preset, m.op.err = presets[i].model.ID, fallbackEffort(presets[i].model), i, ""
		}
	}
	for i := range min(len(m.choices), len(m.models)) {
		if m.choices[i].Clicked(gtx) {
			m.pick(m.models[i])
			m.preset = -1
		}
	}
	model, _ := m.chosen()
	levels := efforts(model)
	for i := range min(len(m.efforts), len(levels)) {
		if m.efforts[i].Clicked(gtx) {
			m.effort, m.preset, m.op.err = levels[i], -1, ""
		}
	}
}

func (m *modelPanel) problem() string {
	model, ok := m.chosen()
	if !ok {
		return "Choose a model."
	}
	if levels := efforts(model); len(levels) > 0 && !slices.Contains(levels, m.effort) {
		return "Choose an effort for " + model.ID + "."
	}
	return ""
}

func (m *modelPanel) save(s *scene) {
	if problem := m.problem(); problem != "" {
		m.op.err = problem
		return
	}
	model, _ := m.chosen()
	edits := []settingEdit{replaceEdit("model", model.ID)}
	if len(efforts(model)) > 0 {
		edits = append(edits, replaceEdit("model_reasoning_effort", m.effort))
	}
	m.op.failed = "Unable to save the model selection: "
	m.op.start(s, request{endpoint: m.endpoint, method: "config/batchWrite", params: batchWrite{Edits: edits}})
}

func (m *modelPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	m.update(gtx, s)
	if _, ok := m.op.settle(s); ok {
		s.env.Submit(state.RefreshEndpoint{Endpoint: m.endpoint})
		s.announce(platform.AnnounceCompletion, "Model set to "+m.model)
		s.close()
	}
	content := m.picker.rows(s, "Endpoint", m.endpoint)
	content = append(content, m.body(s)...)
	content = append(content, m.op.status(s, "Saving the model")...)
	actions := []layout.Widget{m.op.button(s, "Use model", "Saving", Point)}
	if m.listed && len(m.models) == 0 {
		actions = []layout.Widget{func(gtx layout.Context) layout.Dimensions {
			return m.login.Layout(gtx, s.frame, s.env, "Log in", Point)
		}}
	}
	n, row := rows(content)
	return s.show(gtx, Content{Title: titles["model"], Rows: n, Row: row, Dismiss: "Cancel", Actions: actions})
}

func (m *modelPanel) update(gtx layout.Context, s *scene) {
	m.endpoint = defaultEndpoint(s.env, m.endpoint)
	if id, changed := m.picker.update(gtx, s.env.View.Endpoints(), m.endpoint); changed {
		m.release(s.out)
		*m = modelPanel{endpoint: id, preset: -1, picker: m.picker}
	}
	if m.retry.Clicked(gtx) || !m.list.running() && !m.listed && m.loadErr == "" {
		m.load(s)
	}
	m.settle(s)
	if m.login.Clicked(gtx) {
		s.env.Open(shell.Route{Panel: "login", Thread: state.ThreadKey{Endpoint: m.endpoint}})
	}
	m.choose(gtx)
	if m.op.run.Clicked(gtx) && !m.op.call.running() {
		m.save(s)
	}
}

func (m *modelPanel) body(s *scene) []layout.Widget {
	switch {
	case m.loadErr != "":
		return []layout.Widget{
			s.alert("Unable to list models: " + m.loadErr),
			func(gtx layout.Context) layout.Dimensions {
				return m.retry.Layout(gtx, s.frame, s.env, "Try again", Secondary)
			},
		}
	case !m.listed || !m.read:
		return []layout.Widget{s.note("Loading models…")}
	case len(m.models) == 0:
		return []layout.Widget{s.para("No models. Log in to a provider to list its models.")}
	}
	var out []layout.Widget
	out = append(out, m.presetWidgets(s)...)
	out = append(out, s.legend("Models"))
	for len(m.choices) < len(m.models) {
		m.choices = append(m.choices, Choice{})
	}
	for i, model := range m.models {
		o := Option{Label: modelLabel(model), Description: model.Description, Selected: model.ID == m.model}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return m.choices[i].Layout(gtx, s.frame, s.env, o) })
	}
	return append(out, m.effortWidgets(s)...)
}

func modelLabel(model rpc.Model) string {
	if model.DisplayName != "" {
		return model.DisplayName
	}
	return model.ID
}

func (m *modelPanel) presetWidgets(s *scene) []layout.Widget {
	presets := m.presetRows()
	if len(presets) == 0 {
		return nil
	}
	for len(m.presets) < len(presets) {
		m.presets = append(m.presets, Choice{})
	}
	out := []layout.Widget{s.legend("Presets")}
	for i, p := range presets {
		label := p.name + " · " + modelLabel(p.model)
		if effort := fallbackEffort(p.model); effort != "" {
			label += " · " + effort
		}
		o := Option{Label: label, Selected: m.preset == i}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return m.presets[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}

func (m *modelPanel) effortWidgets(s *scene) []layout.Widget {
	model, ok := m.chosen()
	if !ok || len(model.SupportedReasoningEfforts) == 0 {
		return nil
	}
	for len(m.efforts) < len(model.SupportedReasoningEfforts) {
		m.efforts = append(m.efforts, Choice{})
	}
	out := []layout.Widget{s.legend("Effort")}
	for i, e := range model.SupportedReasoningEfforts {
		o := Option{Label: e.ReasoningEffort, Description: e.Description, Selected: e.ReasoningEffort == m.effort}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return m.efforts[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}
