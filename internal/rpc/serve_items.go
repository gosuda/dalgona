package rpc

import (
	"bytes"
	"cmp"
	"encoding/json/jsontext"
	"strings"
)

const (
	kindUser       = "userMessage"
	kindAgent      = "agentMessage"
	kindReasoning  = "reasoning"
	kindCommand    = "commandExecution"
	kindFileChange = "fileChange"
	kindMCP        = "mcpToolCall"
	kindCompaction = "contextCompaction"
	kindDynamic    = "dynamicToolCall"
	kindUpdate     = "update"
	kindAdd        = "add"
	kindDelete     = "delete"

	statusInProgress = "inProgress"
	statusCompleted  = "completed"
	statusFailed     = "failed"
	statusDeclined   = "declined"
)

func (it Item) MarshalJSONTo(enc *jsontext.Encoder) error { return marshalTo(enc, it) }

func (it Item) write(o *out) {
	o.open()
	o.str("type", it.Type)
	o.str("id", it.ID)
	switch it.Type {
	case kindUser:
		o.key("content")
		o.openArray()
		o.open()
		o.str("type", "text")
		o.str("text", it.Text)
		o.close()
		o.closeArray()
	case kindAgent:
		o.str("text", it.Text)
	case kindReasoning:
		o.empty("summary")
		o.key("content")
		o.openArray()
		if it.Text != "" {
			o.text(it.Text)
		}
		o.closeArray()
	case kindCommand:
		it.writeCommand(o)
	case kindFileChange:
		it.writeFileChange(o)
	case kindMCP:
		it.writeMCP(o)
	case kindDynamic:
		it.writeDynamic(o)
	}
	o.close()
}

func (it Item) writeCommand(o *out) {
	o.str("command", it.Command)
	o.empty("commandActions")
	o.str("cwd", it.Cwd)
	o.str("source", "agent")
	o.str("status", it.Status)
	if it.Status != statusInProgress {
		o.str("aggregatedOutput", it.Output)
	}
	if it.ExitCode != nil {
		o.int("exitCode", int64(*it.ExitCode))
	}
	it.writeDuration(o)
	it.writeSpill(o)
}

func (it Item) writeFileChange(o *out) {
	o.key("changes")
	o.openArray()
	for _, change := range it.Changes {
		change.write(o)
	}
	o.closeArray()
	o.str("status", it.Status)
}

func (c FileChange) write(o *out) {
	o.open()
	o.str("path", c.Path)
	o.key("kind")
	o.open()
	o.str("type", c.Kind)
	o.close()
	o.str("diff", c.Diff)
	if c.DiffOmitted {
		o.boolean("diffOmitted", true)
	} else {
		o.int("additions", int64(c.Additions))
		o.int("deletions", int64(c.Deletions))
	}
	o.close()
}

func (it Item) writeMCP(o *out) {
	o.str("server", it.Server)
	o.str("tool", it.Tool)
	o.raw("arguments", it.Arguments)
	o.str("status", toolStatus(it.Status))
	it.writeDuration(o)
	it.writeSpill(o)
	switch it.Status {
	case statusCompleted:
		o.key("result")
		o.open()
		o.key("content")
		o.openArray()
		o.open()
		o.str("type", "text")
		o.str("text", it.Output)
		o.close()
		o.closeArray()
		o.close()
	case statusFailed, statusDeclined:
		o.key("error")
		o.open()
		o.str("message", it.Output)
		o.close()
	}
}

func (it Item) writeDynamic(o *out) {
	o.str("tool", it.Tool)
	o.raw("arguments", it.Arguments)
	o.str("status", toolStatus(it.Status))
	it.writeDuration(o)
	it.writeSpill(o)
	if it.Status == statusInProgress {
		return
	}
	o.boolean("success", it.Status == statusCompleted)
	o.key("contentItems")
	o.openArray()
	o.open()
	o.str("type", "inputText")
	o.str("text", it.Output)
	o.close()
	o.closeArray()
}

func (it Item) writeDuration(o *out) {
	if it.DurationMs != nil {
		o.int("durationMs", *it.DurationMs)
	}
}

func (it Item) writeSpill(o *out) {
	if it.Spill == nil {
		return
	}
	o.key("outputSpill")
	o.open()
	o.str("path", it.Spill.Path)
	o.int("bytes", it.Spill.Bytes)
	o.str("sha256", it.Spill.SHA256)
	o.close()
}

func toolStatus(status string) string {
	if status == statusDeclined {
		return statusFailed
	}
	return status
}

func toolKind(tool, class string) string {
	switch class {
	case "exec":
		return kindCommand
	case "edit":
		return kindFileChange
	case "remote":
		return kindMCP
	case "":
		return toolKindByName(tool)
	}
	return kindDynamic
}

func toolKindByName(tool string) string {
	switch tool {
	case "exec":
		return kindCommand
	case "patch", "apply_patch":
		return kindFileChange
	}
	if strings.Contains(tool, "__") {
		return kindMCP
	}
	return kindDynamic
}

func argString(args jsontext.Value, key string) string {
	var value string
	_ = decodeParams(args, func(name string, d *jsontext.Decoder) error {
		if name != key {
			return d.SkipValue()
		}
		v, err := readString(d)
		value = v
		return err
	})
	return value
}

type itemDecoder struct {
	it      *Item
	content []string
	summary []string
}

func (it *Item) UnmarshalJSONFrom(d *jsontext.Decoder) error {
	*it = Item{}
	state := itemDecoder{it: it}
	if err := readMembers(d, state.member); err != nil {
		return err
	}
	state.finish()
	return nil
}

func (s *itemDecoder) finish() {
	parts := s.content
	if len(parts) == 0 {
		parts = s.summary
	}
	if len(parts) == 0 {
		return
	}
	text := strings.Join(parts, "\n")
	switch s.it.Type {
	case kindUser, kindReasoning, kindAgent:
		s.it.Text = cmp.Or(s.it.Text, text)
	default:
		s.it.Output = cmp.Or(s.it.Output, text)
	}
}

func (s *itemDecoder) member(name string, d *jsontext.Decoder) error {
	if target := s.it.stringField(name); target != nil {
		v, err := readString(d)
		*target = v
		return err
	}
	if handled, err := s.number(name, d); handled {
		return err
	}
	switch name {
	case "arguments":
		v, err := d.ReadValue()
		s.it.Arguments = jsontext.Value(bytes.Clone(v))
		return err
	case "content", "contentItems":
		return readParts(d, &s.content)
	case "summary":
		return readParts(d, &s.summary)
	case "changes":
		return readArray(d, func() error { return s.it.readChange(d) })
	case "result":
		return readMembers(d, s.resultMember)
	case "error":
		return readMembers(d, s.it.textMember("message"))
	}
	return d.SkipValue()
}

func (s *itemDecoder) number(name string, d *jsontext.Decoder) (bool, error) {
	switch name {
	case "exitCode":
		return true, s.it.readExit(d)
	case "durationMs":
		return true, s.it.readDuration(d)
	case "outputSpill":
		return true, s.it.readSpill(d)
	}
	return false, nil
}

func (s *itemDecoder) resultMember(name string, d *jsontext.Decoder) error {
	if name != "content" {
		return d.SkipValue()
	}
	return readParts(d, &s.content)
}

var itemStringFields = map[string]func(*Item) *string{
	"type":             func(it *Item) *string { return &it.Type },
	"id":               func(it *Item) *string { return &it.ID },
	"text":             func(it *Item) *string { return &it.Text },
	"command":          func(it *Item) *string { return &it.Command },
	"cwd":              func(it *Item) *string { return &it.Cwd },
	"status":           func(it *Item) *string { return &it.Status },
	"aggregatedOutput": func(it *Item) *string { return &it.Output },
	"server":           func(it *Item) *string { return &it.Server },
	"tool":             func(it *Item) *string { return &it.Tool },
}

func (it *Item) stringField(name string) *string {
	if field, ok := itemStringFields[name]; ok {
		return field(it)
	}
	return nil
}

func (it *Item) readExit(d *jsontext.Decoder) error {
	if d.PeekKind() == 'n' {
		_, err := d.ReadToken()
		return err
	}
	code, err := readInt(d)
	it.ExitCode = &code
	return err
}

func (it *Item) readDuration(d *jsontext.Decoder) error {
	if d.PeekKind() == 'n' {
		_, err := d.ReadToken()
		return err
	}
	ms, err := readInt(d)
	value := int64(ms)
	it.DurationMs = &value
	return err
}

func (it *Item) readSpill(d *jsontext.Decoder) error {
	if d.PeekKind() == 'n' {
		_, err := d.ReadToken()
		return err
	}
	var spill OutputSpill
	err := readMembers(d, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "path":
			spill.Path, err = readString(d)
		case "bytes":
			var n int
			n, err = readInt(d)
			spill.Bytes = int64(n)
		case "sha256":
			spill.SHA256, err = readString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	it.Spill = &spill
	return err
}

func readParts(d *jsontext.Decoder, parts *[]string) error {
	return readArray(d, func() error {
		if d.PeekKind() == '"' {
			v, err := readString(d)
			*parts = append(*parts, v)
			return err
		}
		var text string
		err := readMembers(d, func(name string, d *jsontext.Decoder) error {
			if name != "text" {
				return d.SkipValue()
			}
			v, err := readString(d)
			text = v
			return err
		})
		*parts = append(*parts, text)
		return err
	})
}

func (it *Item) textMember(key string) memberFunc {
	return func(name string, d *jsontext.Decoder) error {
		if name != key {
			return d.SkipValue()
		}
		v, err := readString(d)
		it.Output = v
		return err
	}
}

func (it *Item) readChange(d *jsontext.Decoder) error {
	var change FileChange
	err := readMembers(d, func(name string, d *jsontext.Decoder) error {
		switch name {
		case "path":
			v, err := readString(d)
			change.Path = v
			return err
		case "diff":
			v, err := readString(d)
			change.Diff = v
			return err
		case "additions":
			v, err := readInt(d)
			change.Additions = v
			return err
		case "deletions":
			v, err := readInt(d)
			change.Deletions = v
			return err
		case "diffOmitted":
			v, err := readBool(d)
			change.DiffOmitted = v
			return err
		case "kind":
			return readMembers(d, func(name string, d *jsontext.Decoder) error {
				if name != "type" {
					return d.SkipValue()
				}
				v, err := readString(d)
				change.Kind = v
				return err
			})
		}
		return d.SkipValue()
	})
	it.Changes = append(it.Changes, change)
	return err
}
