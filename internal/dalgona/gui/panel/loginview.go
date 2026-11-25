//go:build gui

package panel

import (
	"gioui.org/layout"
)

var authProgress = map[string]string{
	authAwaiting: "Starting the sign-in…",
	"waiting":    "Waiting for the sign-in to finish…",
	"exchanging": "Finishing the sign-in…",
}

func (l *loginPanel) providerRows(s *scene) []layout.Widget {
	switch {
	case l.listErr != "":
		return []layout.Widget{
			s.alert("Unable to list providers: " + l.listErr),
			l.button(s, &l.retry, "Try again", Secondary),
		}
	case !l.loaded:
		return []layout.Widget{s.note("Loading providers…")}
	case len(l.targets) == 0:
		return []layout.Widget{s.para("This endpoint offers no sign-in providers.")}
	}
	for len(l.choices) < len(l.targets) {
		l.choices = append(l.choices, Choice{})
	}
	out := []layout.Widget{s.legend("Provider")}
	for i, d := range l.targets {
		o := Option{Label: d.DisplayName, Marker: "Not signed in", Selected: targetKey(d.Target) == l.picked}
		if d.SignedIn {
			o.Marker = "Signed in"
		}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return l.choices[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}

func (l *loginPanel) button(s *scene, b *Button, label string, kind Kind) layout.Widget {
	return func(gtx layout.Context) layout.Dimensions { return b.Layout(gtx, s.frame, s.env, label, kind) }
}

func (l *loginPanel) detailRows(s *scene, d authDescriptor) []layout.Widget {
	account := "Not signed in."
	if d.SignedIn {
		account = "Signed in."
		if d.AccountLabel != "" {
			account = "Signed in as " + d.AccountLabel + "."
		}
	}
	out := []layout.Widget{s.legend(d.DisplayName), s.note(account)}
	if d.UnavailableReason != "" {
		out = append(out, s.para("Sign-in is unavailable: "+d.UnavailableReason))
	}
	flows := l.offered(s.env.View, d)
	if len(flows) == 0 && d.UnavailableReason == "" {
		out = append(out, s.para("No sign-in method for "+d.DisplayName+" works from this computer on this endpoint."))
	}
	out = append(out, l.flowChoiceRows(s, flows)...)
	if flow := l.currentFlow(s.env.View, d); flow != "" {
		out = append(out, l.flowRows(s, flow)...)
	}
	if l.done != "" {
		out = append(out, s.para(l.done))
	}
	out = append(out, l.logout.status(s, "Logging out")...)
	if d.SignedIn {
		out = append(out, l.logout.button(s, "Log out", "Logging out", Destructive))
	}
	return out
}

func (l *loginPanel) flowChoiceRows(s *scene, flows []string) []layout.Widget {
	if len(flows) < 2 {
		return nil
	}
	for len(l.flowChoices) < len(flows) {
		l.flowChoices = append(l.flowChoices, Choice{})
	}
	out := []layout.Widget{s.legend("Method")}
	for i, flow := range flows {
		o := Option{Label: flowLabels[flow], Selected: flow == l.flowName || l.flowName == "" && i == 0, Disabled: l.flow.live()}
		out = append(out, func(gtx layout.Context) layout.Dimensions { return l.flowChoices[i].Layout(gtx, s.frame, s.env, o) })
	}
	return out
}

func (l *loginPanel) flowRows(s *scene, flow string) []layout.Widget {
	if flow == flowAPIKey {
		return l.keyRows(s)
	}
	if l.flow.live() && l.flow.flow == flow {
		return l.challengeRows(s)
	}
	out := l.outcomeRows(s, flow)
	return append(out, l.button(s, &l.begin, l.beginLabel(flow), Point))
}

func (l *loginPanel) beginLabel(flow string) string {
	if l.flow.ended && l.flow.flow == flow && !l.flow.succeeded() {
		return "Try again"
	}
	return "Continue"
}

func (l *loginPanel) keyRows(s *scene) []layout.Widget {
	out := []layout.Widget{func(gtx layout.Context) layout.Dimensions { return l.key.layout(gtx, s) }}
	if l.flow.flow == flowAPIKey {
		out = append(out, l.outcomeRows(s, flowAPIKey)...)
	}
	l.begin.Busy = ""
	if l.blocked() {
		l.begin.Busy = "Logging in"
	}
	return append(out, l.button(s, &l.begin, "Log in", Point))
}

func (l *loginPanel) outcomeRows(s *scene, flow string) []layout.Widget {
	if l.flow.flow != flow || !l.flow.began {
		return nil
	}
	var out []layout.Widget
	if l.flow.fault != "" {
		out = append(out, s.alert(l.flow.fault))
	}
	switch l.flow.status.State {
	case authFailed:
		out = append(out, s.alert("Sign-in failed: "+l.flow.status.Error))
	case authCancelled:
		out = append(out, s.para("Sign-in cancelled."))
	case authExpired:
		out = append(out, s.alert("The sign-in expired before it finished."))
	}
	if l.recheck {
		out = append(out, s.note("Checking whether the sign-in finished before offering another attempt."))
	}
	return out
}

func (l *loginPanel) challengeRows(s *scene) []layout.Widget {
	out := []layout.Widget{s.para(authProgress[l.flow.status.State])}
	if c := l.flow.status.Challenge; c != nil {
		out = append(out, l.linkRows(s, c)...)
		if note := expiry(c.ExpiresAt); note != "" {
			out = append(out, s.note(note))
		}
	}
	if l.flow.flow == flowManual && l.flow.id != "" {
		out = append(out, func(gtx layout.Context) layout.Dimensions { return l.paste.layout(gtx, s) })
		if l.flow.fault != "" {
			out = append(out, s.alert(l.flow.fault))
		}
		out = append(out, l.button(s, &l.send, "Submit code", Point))
	}
	if l.openErr != "" {
		out = append(out, s.alert(l.openErr))
	}
	out = append(out, l.button(s, &l.cancel, "Cancel sign-in", Secondary))
	return append(out, s.note("Closing this panel cancels the sign-in."))
}

func (l *loginPanel) linkRows(s *scene, c *authChallenge) []layout.Widget {
	var out []layout.Widget
	link := c.AuthorizationURL
	if link == "" {
		link = c.VerificationURL
	}
	if c.Prompt != "" {
		out = append(out, s.para(c.Prompt))
	}
	if link != "" {
		l.openLink.Busy = ""
		if l.opening {
			l.openLink.Busy = "Opening"
		}
		out = append(out, s.code(link), l.button(s, &l.openLink, openLabel(l.flow.flow), Secondary), l.button(s, &l.copyLink, "Copy link", Quiet))
	}
	if c.UserCode != "" {
		out = append(out, s.para("Enter this code: "+c.UserCode), l.button(s, &l.copyCode, "Copy code", Quiet))
	}
	return out
}

func openLabel(flow string) string {
	if flow == flowCursor {
		return "Open Cursor"
	}
	return "Open in browser"
}
