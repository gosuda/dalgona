package vtstate

import (
	"bytes"
	"image/color"
	"strings"

	"github.com/charmbracelet/x/ansi"
)

const (
	MaxLinkURIBytes = 2083
	MaxLinkIDBytes  = 250
)

func (t *Terminal) handleOsc(cmd int, data []byte) {
	t.commitCluster()
	switch cmd {
	case 8:
		t.hyperlink(data)
	case 10, 11, 12:
		t.colorQuery(cmd, data)
	}
}

func (t *Terminal) hyperlink(data []byte) {
	parts := bytes.SplitN(data, []byte{';'}, 3)
	if len(parts) != 3 {
		return
	}
	link, ok := parseLink(string(parts[1]), string(parts[2]))
	switch {
	case !ok:
		t.link = nil
	case t.link == nil || *t.link != link:
		t.link = &link
	}
}

func parseLink(params, uri string) (Link, bool) {
	id := ""
	for param := range strings.SplitSeq(params, ":") {
		if value, found := strings.CutPrefix(param, "id="); found {
			id = value
		}
	}
	link := Link{URI: uri, ID: id}
	return link, link.allowed()
}

func (l Link) allowed() bool {
	if len(l.URI) > MaxLinkURIBytes || len(l.ID) > MaxLinkIDBytes || !printableASCII(l.URI, "") || !printableASCII(l.ID, ";:") {
		return false
	}
	scheme, rest, found := strings.Cut(l.URI, ":")
	if !found {
		return false
	}
	switch strings.ToLower(scheme) {
	case "https", "http":
		return len(rest) > 2 && strings.HasPrefix(rest, "//")
	case "mailto":
		return rest != ""
	}
	return false
}

func printableASCII(s, excluded string) bool {
	for i := range len(s) {
		if s[i] <= ' ' || s[i] >= 0x7f || strings.IndexByte(excluded, s[i]) >= 0 {
			return false
		}
	}
	return true
}

var defaultColors = map[int]color.Color{
	10: color.White,
	11: color.Black,
	12: color.White,
}

func (t *Terminal) colorQuery(cmd int, data []byte) {
	parts := bytes.Split(data, []byte{';'})
	if len(parts) != 2 || string(parts[1]) != "?" {
		return
	}
	value := ansi.XRGBColor{Color: defaultColors[cmd]}.String()
	switch cmd {
	case 10:
		t.reply(ansi.SetForegroundColor(value))
	case 11:
		t.reply(ansi.SetBackgroundColor(value))
	case 12:
		t.reply(ansi.SetCursorColor(value))
	}
}
