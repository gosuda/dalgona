//go:build gui

package panel

import (
	"gioui.org/layout"

	"github.com/gosuda/dalgona/internal/dalgona/client"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
)

type endpointPicker struct {
	options []Choice
}

func (e *endpointPicker) update(gtx layout.Context, eps []state.EndpointView, current client.EndpointID) (client.EndpointID, bool) {
	for i := range min(len(e.options), len(eps)) {
		if e.options[i].Clicked(gtx) && eps[i].ID != current {
			return eps[i].ID, true
		}
	}
	return current, false
}

func (e *endpointPicker) rows(s *scene, legend string, current client.EndpointID) []layout.Widget {
	if len(s.env.View.Endpoints()) < 2 {
		return []layout.Widget{s.note(legend + ": " + endpointName(s.env.View, current))}
	}
	return e.choices(s, legend, current)
}

func (e *endpointPicker) choices(s *scene, legend string, current client.EndpointID) []layout.Widget {
	eps := s.env.View.Endpoints()
	for len(e.options) < len(eps) {
		e.options = append(e.options, Choice{})
	}
	out := []layout.Widget{s.legend(legend)}
	for i, ep := range eps {
		label := ep.Label
		if label == "" {
			label = string(ep.ID)
		}
		o := Option{Label: label, Marker: phaseWord(ep.Phase), Selected: ep.ID == current}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return e.options[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}
