package platform

import (
	"errors"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

var ErrComposing = errors.New("text composition is active; finish or cancel the composition first")

type KeyPress struct {
	Name      string
	Composing bool
}

type Composer struct {
	policy   inputtext.Policy
	draft    inputtext.Draft
	preedit  string
	active   bool
	rejected string
}

func NewComposer(policy inputtext.Policy, draft inputtext.Draft) (*Composer, error) {
	if err := inputtext.CheckRange(draft.Text, draft.Selection); err != nil {
		return nil, err
	}
	if err := policy.Check(draft.Text); err != nil {
		return nil, err
	}
	return &Composer{policy: policy, draft: draft}, nil
}

func (c *Composer) Draft() inputtext.Draft { return c.draft }

func (c *Composer) Preedit() string { return c.preedit }

func (c *Composer) Composing() bool { return c.active }

func (c *Composer) TakeRejected() string {
	candidate := c.rejected
	c.rejected = ""
	return candidate
}

func (c *Composer) Owns(k KeyPress) bool { return c.active || k.Composing }

func (c *Composer) Compose(preedit string) {
	c.active, c.preedit = true, preedit
}

func (c *Composer) Cancel() {
	c.active, c.preedit = false, ""
}

func (c *Composer) Commit(text string) error {
	c.Cancel()
	return c.apply(text)
}

func (c *Composer) Replace(candidate string) error {
	if c.active {
		return ErrComposing
	}
	return c.apply(candidate)
}

func (c *Composer) Select(r inputtext.Range) error {
	if c.active {
		return ErrComposing
	}
	if err := inputtext.CheckRange(c.draft.Text, r); err != nil {
		return err
	}
	c.draft.Selection = r
	return nil
}

func (c *Composer) apply(candidate string) error {
	next, err := c.draft.Replace(c.policy, candidate)
	if err != nil {
		c.rejected = candidate
		return err
	}
	c.draft = next
	return nil
}
