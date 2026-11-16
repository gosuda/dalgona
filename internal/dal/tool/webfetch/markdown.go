package webfetch

import (
	"bytes"
	"errors"
	"fmt"
	"net/url"
	"slices"
	"strings"

	readability "codeberg.org/readeck/go-readability/v2"
	htmltomarkdown "github.com/JohannesKaufmann/html-to-markdown/v2"
	"golang.org/x/net/html"
	"golang.org/x/net/html/atom"
)

const titleEscapes = "\\`*_[]()<>#&!|~"

var errNoReadableContent = errors.New("extract article: no readable content")

func htmlMarkdown(doc *html.Node, pageURL *url.URL) ([]byte, error) {
	article, err := readability.FromDocument(doc, pageURL)
	if err != nil {
		return nil, fmt.Errorf("extract article: %w", err)
	}
	if article.Node == nil {
		return nil, errNoReadableContent
	}
	sanitizeLinks(article.Node, pageURL)
	markdown, err := htmltomarkdown.ConvertNode(article.Node)
	if err != nil {
		return nil, fmt.Errorf("convert article to Markdown: %w", err)
	}
	result := withTitle(bytes.TrimSpace(markdown), article.Node, article.Title())
	if len(result) == 0 {
		return nil, errNoReadableContent
	}
	return result, nil
}

func sanitizeLinks(root *html.Node, base *url.URL) {
	var refs []*html.Node
	for node := range root.Descendants() {
		if node.Type == html.ElementNode && (node.DataAtom == atom.A || node.DataAtom == atom.Img) {
			refs = append(refs, node)
		}
	}
	for _, node := range refs {
		sanitizeRef(node, base)
	}
}

func sanitizeRef(node *html.Node, base *url.URL) {
	key := "href"
	if node.DataAtom == atom.Img {
		key = "src"
	}
	for i := range node.Attr {
		if node.Attr[i].Namespace != "" || node.Attr[i].Key != key {
			continue
		}
		if resolved, ok := resolveHTTP(base, node.Attr[i].Val); ok {
			node.Attr[i].Val = resolved
		} else {
			node.Attr = slices.Delete(node.Attr, i, i+1)
		}
		return
	}
}

func resolveHTTP(base *url.URL, raw string) (string, bool) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return "", false
	}
	ref, err := url.Parse(raw)
	if err != nil {
		return "", false
	}
	resolved := base.ResolveReference(ref)
	if resolved.Scheme != "http" && resolved.Scheme != "https" || resolved.Host == "" {
		return "", false
	}
	return resolved.String(), true
}

func withTitle(markdown []byte, root *html.Node, title string) []byte {
	title = normalizeSpace(title)
	if title == "" || slices.Contains(headingTexts(root), title) {
		return markdown
	}
	return slices.Concat([]byte("# "+escapeTitle(title)+"\n\n"), markdown)
}

func headingTexts(root *html.Node) []string {
	var texts []string
	for node := range root.Descendants() {
		if node.Type == html.ElementNode && isHeading(node.DataAtom) {
			texts = append(texts, normalizeSpace(nodeText(node)))
		}
	}
	return texts
}

func isHeading(name atom.Atom) bool {
	return name == atom.H1 || name == atom.H2 || name == atom.H3 || name == atom.H4 || name == atom.H5 || name == atom.H6
}

func nodeText(node *html.Node) string {
	var text strings.Builder
	for descendant := range node.Descendants() {
		if descendant.Type == html.TextNode {
			text.WriteString(descendant.Data)
		}
	}
	return text.String()
}

func normalizeSpace(text string) string {
	return strings.Join(strings.Fields(text), " ")
}

func escapeTitle(title string) string {
	var escaped strings.Builder
	for _, r := range title {
		if strings.ContainsRune(titleEscapes, r) {
			escaped.WriteByte('\\')
		}
		escaped.WriteRune(r)
	}
	return escaped.String()
}
