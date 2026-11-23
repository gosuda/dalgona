package rpc

import (
	"bytes"
	"cmp"
	"encoding/json/jsontext"
	"strings"
)

const (
	KindUser       = "userMessage"
	KindAgent      = "agentMessage"
	KindReasoning  = "reasoning"
	KindCommand    = "commandExecution"
	KindFileChange = "fileChange"
	KindMCP        = "mcpToolCall"
	KindCompaction = "contextCompaction"
	KindDynamic    = "dynamicToolCall"
	KindUpdate     = "update"
	KindAdd        = "add"
	KindDelete     = "delete"

	StatusInProgress = "inProgress"
	StatusCompleted  = "completed"
	StatusFailed     = "failed"
	StatusDeclined   = "declined"
)

func (it Item) MarshalJSONTo(enc *jsontext.Encoder) error { return marshalTo(enc, it) }

func (it Item) writeUserContent(o *Writer) {
	o.Key("content")
	o.OpenArray()
	o.Open()
	o.Str("type", "text")
	o.Str("text", it.Text)
	o.Close()
	for _, image := range it.Images {
		image.Encode(o)
	}
	o.CloseArray()
}

func (it Item) Encode(o *Writer) {
	o.Open()
	o.Str("type", it.Type)
	o.Str("id", it.ID)
	switch it.Type {
	case KindUser:
		it.writeUserContent(o)
		it.writeKeywords(o)
	case KindAgent:
		o.Str("text", it.Text)
	case KindReasoning:
		o.Empty("summary")
		o.Key("content")
		o.OpenArray()
		if it.Text != "" {
			o.Text(it.Text)
		}
		o.CloseArray()
	case KindCommand:
		it.writeCommand(o)
	case KindFileChange:
		it.writeFileChange(o)
	case KindMCP:
		it.writeMCP(o)
	case KindDynamic:
		it.writeDynamic(o)
	}
	o.Close()
}

func (it Item) writeCommand(o *Writer) {
	o.Str("command", it.Command)
	o.Empty("commandActions")
	o.Str("cwd", it.Cwd)
	o.Str("source", "agent")
	o.Str("status", it.Status)
	if it.Status != StatusInProgress {
		o.Str("aggregatedOutput", it.Output)
	}
	if it.ExitCode != nil {
		o.Int("exitCode", int64(*it.ExitCode))
	}
	it.writeDuration(o)
	it.writeSpill(o)
}

func (it Item) writeFileChange(o *Writer) {
	o.Key("changes")
	o.OpenArray()
	for _, change := range it.Changes {
		change.Encode(o)
	}
	o.CloseArray()
	o.Str("status", it.Status)
}

func (c FileChange) Encode(o *Writer) {
	o.Open()
	o.Str("path", c.Path)
	o.Key("kind")
	o.Open()
	o.Str("type", c.Kind)
	o.Close()
	o.Str("diff", c.Diff)
	if c.DiffOmitted {
		o.Bool("diffOmitted", true)
	} else {
		o.Int("additions", int64(c.Additions))
		o.Int("deletions", int64(c.Deletions))
	}
	o.Close()
}

func (it Item) writeMCP(o *Writer) {
	o.Str("server", it.Server)
	o.Str("tool", it.Tool)
	o.Raw("arguments", it.Arguments)
	o.Str("status", toolStatus(it.Status))
	it.writeDuration(o)
	it.writeSpill(o)
	switch it.Status {
	case StatusCompleted:
		o.Key("result")
		o.Open()
		o.Key("content")
		o.OpenArray()
		o.Open()
		o.Str("type", "text")
		o.Str("text", it.Output)
		o.Close()
		o.CloseArray()
		o.Close()
	case StatusFailed, StatusDeclined:
		o.Key("error")
		o.Open()
		o.Str("message", it.Output)
		o.Close()
	}
}

func (it Item) writeDynamic(o *Writer) {
	o.Str("tool", it.Tool)
	o.Raw("arguments", it.Arguments)
	o.Str("status", toolStatus(it.Status))
	it.writeDuration(o)
	it.writeSpill(o)
	if it.Status == StatusInProgress {
		return
	}
	o.Bool("success", it.Status == StatusCompleted)
	o.Key("contentItems")
	o.OpenArray()
	o.Open()
	o.Str("type", "inputText")
	o.Str("text", it.Output)
	o.Close()
	o.CloseArray()
}

func (it Item) writeDuration(o *Writer) {
	if it.DurationMs != nil {
		o.Int("durationMs", *it.DurationMs)
	}
}

func (it Item) writeSpill(o *Writer) {
	if it.Spill == nil {
		return
	}
	o.Key("outputSpill")
	o.Open()
	o.Str("path", it.Spill.Path)
	o.Int("bytes", it.Spill.Bytes)
	o.Str("sha256", it.Spill.SHA256)
	o.Close()
}

func (it Item) writeKeywords(o *Writer) {
	if len(it.Keywords) == 0 {
		return
	}
	o.Key("keywords")
	o.OpenArray()
	for _, match := range it.Keywords {
		match.Encode(o)
	}
	o.CloseArray()
}

func toolStatus(status string) string {
	if status == StatusDeclined {
		return StatusFailed
	}
	return status
}

type itemDecoder struct {
	it      *Item
	content []string
	summary []string
}

func (it *Item) UnmarshalJSONFrom(d *jsontext.Decoder) error {
	*it = Item{}
	state := itemDecoder{it: it}
	if err := ReadMembers(d, state.member); err != nil {
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
	case KindUser, KindReasoning, KindAgent:
		s.it.Text = cmp.Or(s.it.Text, text)
	default:
		s.it.Output = cmp.Or(s.it.Output, text)
	}
}

func (s *itemDecoder) member(name string, d *jsontext.Decoder) error {
	if target := s.it.stringField(name); target != nil {
		v, err := ReadString(d)
		*target = v
		return err
	}
	if handled, err := s.typed(name, d); handled {
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
		return ReadArray(d, func() error { return s.it.readChange(d) })
	case "result":
		return ReadMembers(d, s.resultMember)
	case "error":
		return ReadMembers(d, s.it.textMember("message"))
	}
	return d.SkipValue()
}

func (s *itemDecoder) typed(name string, d *jsontext.Decoder) (bool, error) {
	switch name {
	case "exitCode":
		return true, s.it.readExit(d)
	case "durationMs":
		return true, s.it.readDuration(d)
	case "outputSpill":
		return true, s.it.readSpill(d)
	case "keywords":
		return true, s.it.readKeywords(d)
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
	code, err := ReadInt(d)
	it.ExitCode = &code
	return err
}

func (it *Item) readDuration(d *jsontext.Decoder) error {
	if d.PeekKind() == 'n' {
		_, err := d.ReadToken()
		return err
	}
	ms, err := ReadInt(d)
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
	err := ReadMembers(d, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "path":
			spill.Path, err = ReadString(d)
		case "bytes":
			var n int
			n, err = ReadInt(d)
			spill.Bytes = int64(n)
		case "sha256":
			spill.SHA256, err = ReadString(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
	it.Spill = &spill
	return err
}

func (it *Item) readKeywords(d *jsontext.Decoder) error {
	if d.PeekKind() == 'n' {
		_, err := d.ReadToken()
		return err
	}
	return ReadArray(d, func() error {
		var match KeywordMatch
		err := ReadMembers(d, func(name string, d *jsontext.Decoder) error {
			return readKeywordMember(name, d, &match)
		})
		it.Keywords = append(it.Keywords, match)
		return err
	})
}

func readKeywordMember(name string, d *jsontext.Decoder, match *KeywordMatch) error {
	var err error
	switch name {
	case "word":
		match.Word, err = ReadString(d)
	case "byteRange":
		err = readByteRange(d, &match.ByteRange)
	case "hue":
		err = readHue(d, &match.Hue)
	case "effort":
		match.Effort, err = ReadString(d)
	case "summary":
		match.Summary, err = ReadString(d)
	default:
		err = d.SkipValue()
	}
	return err
}

func readByteRange(d *jsontext.Decoder, r *ByteRange) error {
	return ReadMembers(d, func(name string, d *jsontext.Decoder) error {
		var err error
		switch name {
		case "start":
			r.Start, err = ReadInt(d)
		case "end":
			r.End, err = ReadInt(d)
		default:
			err = d.SkipValue()
		}
		return err
	})
}

func readHue(d *jsontext.Decoder, hue *[2]int) error {
	var values []int
	err := ReadArray(d, func() error {
		n, err := ReadInt(d)
		values = append(values, n)
		return err
	})
	copy(hue[:], values)
	return err
}

func readParts(d *jsontext.Decoder, parts *[]string) error {
	return ReadArray(d, func() error {
		if d.PeekKind() == '"' {
			v, err := ReadString(d)
			*parts = append(*parts, v)
			return err
		}
		var text string
		err := ReadMembers(d, func(name string, d *jsontext.Decoder) error {
			if name != "text" {
				return d.SkipValue()
			}
			v, err := ReadString(d)
			text = v
			return err
		})
		*parts = append(*parts, text)
		return err
	})
}

func (it *Item) textMember(key string) MemberFunc {
	return func(name string, d *jsontext.Decoder) error {
		if name != key {
			return d.SkipValue()
		}
		v, err := ReadString(d)
		it.Output = v
		return err
	}
}

func (it *Item) readChange(d *jsontext.Decoder) error {
	var change FileChange
	err := ReadMembers(d, func(name string, d *jsontext.Decoder) error {
		switch name {
		case "path":
			v, err := ReadString(d)
			change.Path = v
			return err
		case "diff":
			v, err := ReadString(d)
			change.Diff = v
			return err
		case "additions":
			v, err := ReadInt(d)
			change.Additions = v
			return err
		case "deletions":
			v, err := ReadInt(d)
			change.Deletions = v
			return err
		case "diffOmitted":
			v, err := ReadBool(d)
			change.DiffOmitted = v
			return err
		case "kind":
			return ReadMembers(d, func(name string, d *jsontext.Decoder) error {
				if name != "type" {
					return d.SkipValue()
				}
				v, err := ReadString(d)
				change.Kind = v
				return err
			})
		}
		return d.SkipValue()
	})
	it.Changes = append(it.Changes, change)
	return err
}

func (n *FileChangePatchUpdatedNotification) UnmarshalJSONFrom(d *jsontext.Decoder) error {
	var holder Item
	err := ReadMembers(d, func(name string, d *jsontext.Decoder) error {
		switch name {
		case "threadId":
			v, err := ReadString(d)
			n.ThreadID = v
			return err
		case "turnId":
			v, err := ReadString(d)
			n.TurnID = v
			return err
		case "itemId":
			v, err := ReadString(d)
			n.ItemID = v
			return err
		case "changes":
			return ReadArray(d, func() error { return holder.readChange(d) })
		}
		return d.SkipValue()
	})
	n.Changes = holder.Changes
	return err
}
