//go:build gui

package panel

import (
	"encoding/json/jsontext"
	"encoding/json/v2"
	"slices"

	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

const (
	keyApproval   = "approval"
	keyVerbosity  = "verbosity"
	keyTheme      = "theme"
	keyStatusHide = "statusHide"
	keyEditReview = "editReview"
	hiddenModel   = "model"
)

var settingWords = map[string]string{
	"ask": "Ask", "auto": "Auto", "yolo": "Yolo", "low": "Low", "medium": "Medium", "high": "High",
	"auto-soft": "Auto soft", "day": "Day", "soft-day": "Soft day", "night": "Night", "soft-night": "Soft night",
	"effort": "Effort", "sandbox": "Sandbox", "folder": "Folder", "queue": "Queue", "cost": "Cost",
	"context": "Context", "mark": "Product mark",
}

var enumFields = [...]struct{ key, legend, problem string }{
	{keyApproval, "Approval", "Choose a policy for approvals."},
	{keyVerbosity, "Verbosity", "Choose a level of detail."},
	{keyTheme, "Theme", "Choose a theme."},
}

type wireSetting struct {
	Key     string           `json:"key"`
	Value   jsontext.Value   `json:"value"`
	Options []jsontext.Value `json:"options"`
}

type settingsReply struct {
	Settings []wireSetting `json:"settings"`
}

type settingsDraft struct {
	enums      map[string]string
	statusHide []string
	editReview bool
}

type settingsPanel struct {
	notifications shell.Widget
	endpoint      client.EndpointID
	picker        endpointPicker
	read          pending
	loaded        bool
	readErr       string
	retry         Button
	options       map[string][]string
	saved         settingsDraft
	draft         settingsDraft
	enumChoices   map[string][]Choice
	hide          []Check
	review        Check
	links         [3]Button
	op            operation
}

func (p *settingsPanel) open(r shell.Route) {
	*p = settingsPanel{notifications: p.notifications, picker: p.picker, endpoint: r.Thread.Endpoint}
}

func (p *settingsPanel) release(out *[]state.Intent) {
	p.read.release(out)
	p.op.call.release(out)
}

func wordFor(value string) string {
	if w, ok := settingWords[value]; ok {
		return w
	}
	return value
}

func parseSettings(reply settingsReply) (settingsDraft, map[string][]string) {
	draft := settingsDraft{enums: map[string]string{}}
	options := map[string][]string{}
	for _, s := range reply.Settings {
		var text string
		var list []string
		switch s.Key {
		case keyApproval, keyVerbosity, keyTheme:
			_ = json.Unmarshal(s.Value, &text)
			draft.enums[s.Key] = text
		case keyStatusHide:
			_ = json.Unmarshal(s.Value, &list)
			draft.statusHide = slices.DeleteFunc(list, func(v string) bool { return v == hiddenModel })
		case keyEditReview:
			_ = json.Unmarshal(s.Value, &draft.editReview)
		}
		options[s.Key] = stringOptions(s.Options)
	}
	return draft, options
}

func stringOptions(raw []jsontext.Value) []string {
	out := make([]string, 0, len(raw))
	for _, r := range raw {
		var v string
		if json.Unmarshal(r, &v) == nil && v != hiddenModel {
			out = append(out, v)
		}
	}
	return out
}

func (d settingsDraft) clone() settingsDraft {
	c := settingsDraft{enums: map[string]string{}, statusHide: slices.Clone(d.statusHide), editReview: d.editReview}
	for k, v := range d.enums {
		c.enums[k] = v
	}
	return c
}

func (p *settingsPanel) settleRead(s *scene) {
	v, done := p.read.settle(s)
	if !done {
		return
	}
	reply, err := decodeCall[settingsReply](v)
	if err != nil {
		p.readErr = err.Error()
		return
	}
	p.saved, p.options = parseSettings(reply)
	p.draft, p.loaded = p.saved.clone(), true
}

func (p *settingsPanel) problem() string {
	for _, f := range enumFields {
		if !slices.Contains(p.options[f.key], p.draft.enums[f.key]) {
			return f.problem
		}
	}
	return ""
}

func sameItems(a, b []string) bool {
	return slices.Equal(slices.Sorted(slices.Values(a)), slices.Sorted(slices.Values(b)))
}

func (p *settingsPanel) edits() []settingEdit {
	var out []settingEdit
	for _, f := range enumFields {
		if p.draft.enums[f.key] != p.saved.enums[f.key] {
			out = append(out, replaceEdit(f.key, p.draft.enums[f.key]))
		}
	}
	if !sameItems(p.draft.statusHide, p.saved.statusHide) {
		out = append(out, replaceEdit(keyStatusHide, slices.Clone(p.draft.statusHide)))
	}
	if p.draft.editReview != p.saved.editReview {
		out = append(out, replaceEdit(keyEditReview, p.draft.editReview))
	}
	return out
}

func (p *settingsPanel) save(s *scene) {
	if problem := p.problem(); problem != "" {
		p.op.err = problem
		return
	}
	edits := p.edits()
	if len(edits) == 0 {
		s.close()
		return
	}
	p.op.failed = "Unable to save the settings: "
	p.op.start(s, request{endpoint: p.endpoint, method: "config/batchWrite", params: batchWrite{Edits: edits}})
}

func (p *settingsPanel) choose(gtx layout.Context, s *scene) {
	for _, f := range enumFields {
		choices := p.enumChoices[f.key]
		for i, value := range p.options[f.key] {
			if i < len(choices) && choices[i].Clicked(gtx) {
				p.draft.enums[f.key], p.op.err = value, ""
			}
		}
	}
	items := p.options[keyStatusHide]
	for i := range min(len(p.hide), len(items)) {
		if p.hide[i].Clicked(gtx) {
			p.draft.statusHide, p.op.err = toggled(p.draft.statusHide, items[i]), ""
		}
	}
	if p.review.Clicked(gtx) {
		p.draft.editReview = !p.draft.editReview
	}
	p.follow(gtx, s)
}

func (p *settingsPanel) follow(gtx layout.Context, s *scene) {
	panels := [...]string{"remote", "mcp", "failures"}
	for i := range p.links {
		if p.links[i].Clicked(gtx) {
			s.env.Open(shell.Route{Panel: panels[i]})
		}
	}
}

func toggled(items []string, item string) []string {
	if at := slices.Index(items, item); at >= 0 {
		return slices.Delete(slices.Clone(items), at, at+1)
	}
	return append(slices.Clone(items), item)
}

func (p *settingsPanel) update(gtx layout.Context, s *scene) {
	p.endpoint = defaultEndpoint(s.env, p.endpoint)
	if id, changed := p.picker.update(gtx, s.env.View.Endpoints(), p.endpoint); changed {
		p.release(s.out)
		*p = settingsPanel{notifications: p.notifications, picker: p.picker, endpoint: id}
	}
	p.load(gtx, s)
	if p.loaded {
		p.choose(gtx, s)
	}
	if p.op.run.Clicked(gtx) && p.loaded && !p.op.call.running() {
		p.save(s)
	}
	if _, ok := p.op.settle(s); ok {
		s.env.Submit(state.RefreshEndpoint{Endpoint: p.endpoint})
		s.announce(platform.AnnounceCompletion, "Settings saved")
		s.close()
	}
}

func (p *settingsPanel) load(gtx layout.Context, s *scene) {
	if p.retry.Clicked(gtx) || !p.loaded && !p.read.running() && p.readErr == "" {
		p.readErr = ""
		p.read.start(s, request{endpoint: p.endpoint, method: "dalgona/settings/read", params: struct{}{}})
	}
	p.settleRead(s)
}

func (p *settingsPanel) layout(gtx layout.Context, s *scene) layout.Dimensions {
	p.update(gtx, s)
	content := p.picker.rows(s, "Endpoint", p.endpoint)
	content = append(content, p.body(s)...)
	content = append(content, p.op.status(s, "Saving the settings")...)
	content = append(content, p.linkRows(s)...)
	if p.notifications != nil {
		content = append(content, func(gtx layout.Context) layout.Dimensions { return p.notifications.Layout(gtx, s.env) })
	}
	p.op.run.Disabled = !p.loaded
	n, row := rows(content)
	return s.show(gtx, Content{
		Title: titles["settings"], Rows: n, Row: row, Dismiss: "Cancel",
		Actions: []layout.Widget{p.op.button(s, "Save", "Saving", Point)},
	})
}

func (p *settingsPanel) body(s *scene) []layout.Widget {
	switch {
	case p.readErr != "":
		return []layout.Widget{
			s.alert("Unable to read the settings: " + p.readErr),
			func(gtx layout.Context) layout.Dimensions {
				return p.retry.Layout(gtx, s.frame, s.env, "Try again", Secondary)
			},
		}
	case !p.loaded:
		return []layout.Widget{s.note("Loading settings…")}
	}
	var out []layout.Widget
	for _, f := range enumFields {
		out = append(out, p.enumRows(s, f.key, f.legend)...)
		if f.key == keyApproval && p.draft.enums[keyApproval] != p.saved.enums[keyApproval] {
			out = append(out, s.note("Open threads keep their existing approval and sandbox."))
		}
	}
	out = append(out, p.hideRows(s)...)
	review := Option{Label: "Review edits", Selected: p.draft.editReview, Description: "Sends each edit's masked diff to the decider model."}
	out = append(out, func(gtx layout.Context) layout.Dimensions { return p.review.Layout(gtx, s.frame, s.env, review) })
	return out
}

func (p *settingsPanel) enumRows(s *scene, key, legend string) []layout.Widget {
	if p.enumChoices == nil {
		p.enumChoices = map[string][]Choice{}
	}
	values := p.options[key]
	for len(p.enumChoices[key]) < len(values) {
		p.enumChoices[key] = append(p.enumChoices[key], Choice{})
	}
	choices := p.enumChoices[key]
	out := []layout.Widget{s.legend(legend)}
	for i, value := range values {
		o := Option{Label: wordFor(value), Selected: p.draft.enums[key] == value}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return choices[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}

func (p *settingsPanel) hideRows(s *scene) []layout.Widget {
	items := p.options[keyStatusHide]
	for len(p.hide) < len(items) {
		p.hide = append(p.hide, Check{})
	}
	out := []layout.Widget{s.legend("Hidden status items")}
	for i, item := range items {
		o := Option{Label: wordFor(item), Selected: slices.Contains(p.draft.statusHide, item)}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return p.hide[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}

func (p *settingsPanel) linkRows(s *scene) []layout.Widget {
	labels := [...]string{"Endpoints", "MCP servers", "Failure reporting"}
	out := []layout.Widget{s.legend("More settings")}
	for i, label := range labels {
		out = append(out, func(gtx layout.Context) layout.Dimensions {
			return p.links[i].Layout(gtx, s.frame, s.env, label, Secondary)
		})
	}
	return out
}
