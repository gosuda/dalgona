package webfetch

import (
	"strings"
	"unicode"

	"golang.org/x/net/html"
	"golang.org/x/net/html/atom"
)

const appShellTextLimit = 200

func LooksLikeAppShell(root *html.Node) bool {
	if root == nil || !hasExecutableScript(root) {
		return false
	}
	var counter textCounter
	return !counter.reached(bodyElement(root))
}

func hasExecutableScript(root *html.Node) bool {
	for node := range root.Descendants() {
		if node.Type == html.ElementNode && node.DataAtom == atom.Script && executableScript(node) {
			return true
		}
	}
	return false
}

func executableScript(script *html.Node) bool {
	var kind string
	for _, attr := range script.Attr {
		if attr.Namespace == "" && attr.Key == "type" {
			kind = attr.Val
			break
		}
	}
	kind, _, _ = strings.Cut(kind, ";")
	switch strings.ToLower(strings.TrimSpace(kind)) {
	case "", "text/javascript", "application/javascript", "module":
		return true
	default:
		return false
	}
}

func bodyElement(root *html.Node) *html.Node {
	for node := range root.Descendants() {
		if node.Type == html.ElementNode && node.DataAtom == atom.Body {
			return node
		}
	}
	return root
}

type textCounter struct {
	runes int
	gap   bool
}

func (c *textCounter) reached(node *html.Node) bool {
	if node.Type == html.TextNode {
		c.add(node.Data)
		return c.runes >= appShellTextLimit
	}
	if node.Type == html.ElementNode && hiddenText(node.DataAtom) {
		return false
	}
	for child := range node.ChildNodes() {
		if c.reached(child) {
			return true
		}
	}
	return false
}

func (c *textCounter) add(text string) {
	for _, r := range text {
		if unicode.IsSpace(r) {
			c.gap = true
			continue
		}
		if c.gap && c.runes > 0 {
			c.runes++
		}
		c.gap = false
		c.runes++
	}
}

func hiddenText(name atom.Atom) bool {
	return name == atom.Script || name == atom.Style || name == atom.Noscript || name == atom.Template
}
