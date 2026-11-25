//go:build gui

package composer

type fieldNotice struct {
	text     string
	rejected string
}

func (n *fieldNotice) fail(text string) { n.text, n.rejected = text, "" }

func (n *fieldNotice) reject(text, candidate string) { n.text, n.rejected = text, candidate }

func (n *fieldNotice) clear() { *n = fieldNotice{} }
