//go:build gui

package composer

import (
	"fmt"

	"gioui.org/layout"
	"gioui.org/widget"

	"github.com/gosuda/dalgona/internal/dalgona/gui/giodraw"
	"github.com/gosuda/dalgona/internal/dalgona/gui/platform"
	"github.com/gosuda/dalgona/internal/dalgona/gui/state"
	"github.com/gosuda/dalgona/internal/dalgona/gui/theme"
)

const (
	shellRowsLabel  = "Shell commands"
	shellTransient  = "Not saved to history"
	shellTimeoutCap = "120 s"
)

type shellRows struct {
	toggle map[uint64]*widget.Clickable
	stop   map[uint64]*widget.Clickable
	check  map[uint64]*widget.Clickable
}

func shellClick(m *map[uint64]*widget.Clickable, ticket uint64) *widget.Clickable {
	if *m == nil {
		*m = map[uint64]*widget.Clickable{}
	}
	c := (*m)[ticket]
	if c == nil {
		c = new(widget.Clickable)
		(*m)[ticket] = c
	}
	return c
}

func shellStatus(r state.ShellRun) string {
	switch r.Phase {
	case state.ShellAuthorizing:
		return "Waiting for terminal permission"
	case state.ShellDeclined:
		return "Terminal permission declined; the command did not run"
	case state.ShellRunning:
		return "Running in " + r.Root
	case state.ShellExited:
		return exitStatus(r)
	case state.ShellTimedOut:
		return "Timed out after " + shellTimeoutCap + truncatedNote(r)
	case state.ShellCancelled:
		return "Stopped before it finished" + truncatedNote(r)
	case state.ShellFailed:
		return shellFailure(r)
	case state.ShellUnknown:
		return "Outcome unknown; check it before running the command again"
	}
	return ""
}

func exitStatus(r state.ShellRun) string {
	if r.ExitCode == nil {
		return "Exited without an exit code" + truncatedNote(r)
	}
	_, lines := shellOutput(r.Output)
	return fmt.Sprintf("exit %d, %d lines%s", *r.ExitCode, lines, truncatedNote(r))
}

func truncatedNote(r state.ShellRun) string {
	if !r.Truncated {
		return ""
	}
	return " · output truncated at " + shellOutputCap
}

func hasOutput(r state.ShellRun) bool {
	switch r.Phase {
	case state.ShellExited, state.ShellTimedOut, state.ShellCancelled:
		return r.Output != ""
	case state.ShellAuthorizing, state.ShellDeclined, state.ShellRunning, state.ShellFailed, state.ShellUnknown:
	}
	return false
}

func (s *shellState) update(gtx layout.Context, d *Dock, env frame) {
	for _, r := range env.view.ShellRuns(d.key) {
		if shellClick(&s.rows.toggle, r.Ticket).Clicked(gtx) {
			s.shown[r.Ticket] = !s.shown[r.Ticket]
		}
		if shellClick(&s.rows.stop, r.Ticket).Clicked(gtx) && r.Phase == state.ShellRunning {
			env.Submit(state.StopShell{Key: d.key, Ticket: r.Ticket})
		}
		if shellClick(&s.rows.check, r.Ticket).Clicked(gtx) && r.Phase == state.ShellUnknown {
			env.Submit(state.Check{OperationID: r.OperationID})
		}
	}
}

func (s *shellState) layout(gtx layout.Context, d *Dock, env frame, look dockLook) layout.Dimensions {
	runs := env.view.ShellRuns(d.key)
	if len(runs) == 0 {
		return layout.Dimensions{}
	}
	items := make([]layout.Widget, 0, len(runs))
	for _, r := range runs {
		items = append(items, func(gtx layout.Context) layout.Dimensions { return s.row(gtx, env, look, r) })
	}
	call, dims := giodraw.Record(gtx, func(gtx layout.Context) layout.Dimensions { return column(gtx, theme.Space2, items...) })
	region(gtx, shellRowsLabel, call, dims)
	return dims
}

func (s *shellState) row(gtx layout.Context, env frame, look dockLook, r state.ShellRun) layout.Dimensions {
	title := fmt.Sprintf("Shell #%d · %s", r.N, firstLine(r.Command))
	status := shellStatus(r) + " · " + shellTransient
	head := func(gtx layout.Context) layout.Dimensions {
		return flow(gtx, theme.Space2, s.rowParts(env, look, r, title, status)...)
	}
	if !s.shown[r.Ticket] || !hasOutput(r) {
		return head(gtx)
	}
	output, _ := shellOutput(r.Output)
	body := func(gtx layout.Context) layout.Dimensions {
		st := small(look.palette.Text)
		st.face = platform.CodeFace
		return drawText(gtx, env.Shaper, st, previewLines(output, previewLineLimit))
	}
	return column(gtx, theme.Space1, head, body)
}

func (s *shellState) rowParts(env frame, look dockLook, r state.ShellRun, title, status string) []layout.Widget {
	ink := look.palette.Dim
	if r.Phase == state.ShellFailed || r.Phase == state.ShellDeclined {
		ink = look.palette.FailedText
	}
	parts := []layout.Widget{
		func(gtx layout.Context) layout.Dimensions {
			st := small(look.palette.Text)
			st.face = platform.CodeFace
			return drawText(gtx, env.Shaper, st, title)
		},
		func(gtx layout.Context) layout.Dimensions { return drawText(gtx, env.Shaper, small(ink), status) },
	}
	if action, ok := s.action(r, title); ok {
		parts = append(parts, func(gtx layout.Context) layout.Dimensions {
			return action.layout(gtx, env.Shaper, look.palette, look.point)
		})
	}
	return parts
}

func (s *shellState) action(r state.ShellRun, title string) (control, bool) {
	switch {
	case r.Phase == state.ShellRunning:
		return control{click: shellClick(&s.rows.stop, r.Ticket), label: "Stop", describe: title, variant: variantDestructive}, true
	case r.Phase == state.ShellUnknown:
		return control{click: shellClick(&s.rows.check, r.Ticket), label: "Check outcome", describe: title, variant: variantSecondary}, true
	case hasOutput(r) && s.shown[r.Ticket]:
		return control{click: shellClick(&s.rows.toggle, r.Ticket), label: "Hide output", describe: title, variant: variantQuiet}, true
	case hasOutput(r):
		return control{click: shellClick(&s.rows.toggle, r.Ticket), label: "Show output", describe: title, variant: variantQuiet}, true
	}
	return control{}, false
}
