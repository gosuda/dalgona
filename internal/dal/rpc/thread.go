package rpc

import (
	"encoding/json/jsontext"
)

func (p *ThreadStartParams) ConfigMember(name string, d *jsontext.Decoder) error {
	if name != "model_reasoning_effort" {
		return d.SkipValue()
	}
	if p.Config == nil {
		p.Config = &ThreadStartConfig{}
	}
	v, err := ReadString(d)
	p.Config.ModelReasoningEffort = v
	return err
}

func ThreadIDMember(id *string) MemberFunc {
	return func(name string, d *jsontext.Decoder) error {
		if name != "threadId" {
			return d.SkipValue()
		}
		v, err := ReadString(d)
		*id = v
		return err
	}
}

func (p *ThreadResumeParams) Member(name string, d *jsontext.Decoder) error {
	if name != "excludeTurns" {
		return ThreadIDMember(&p.ThreadID)(name, d)
	}
	v, err := ReadBool(d)
	p.ExcludeTurns = v
	return err
}
