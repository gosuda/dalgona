package advisor

import (
	"bytes"
	"html"
	"iter"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const (
	advisoryOpen  = "<advisory "
	advisoryClose = "</advisory>"
	escapedBytes  = "&<>"
)

func Render(events []event.Seqd) string {
	var out strings.Builder
	for _, item := range events {
		renderEvent(&out, item.Event)
	}
	return out.String()
}

func renderEvent(out *strings.Builder, e event.Event) {
	switch e := e.(type) {
	case event.UserMessage:
		renderUser(out, e)
	case event.AssistantMessage:
		renderAssistant(out, e)
	case event.ToolCompleted:
		renderToolResult(out, e)
	case event.Compaction:
		writeElement(out, "<summary>", e.Summary, nil, "</summary>")
	case event.TurnCompleted:
		renderTurnEnd(out, e)
	}
}

func renderUser(out *strings.Builder, e event.UserMessage) {
	if e.TextBlob != nil {
		writeElement(out, "<user>", "", e.TextBlob, "</user>")
		return
	}
	separator := "<user>\n"
	for segment := range userSegments(e.Text) {
		segment = strings.TrimSpace(segment)
		if segment == "" {
			continue
		}
		out.WriteString(separator)
		writeText(out, segment)
		separator = "\n"
	}
	if separator == "\n" {
		out.WriteString("\n</user>\n")
	}
}

func userSegments(text string) iter.Seq[string] {
	return func(yield func(string) bool) {
		rest := text
		for {
			before, after, opened := strings.Cut(rest, advisoryOpen)
			if !opened {
				break
			}
			_, tail, closed := strings.Cut(after, advisoryClose)
			if !closed {
				break
			}
			if !yield(before) {
				return
			}
			rest = tail
		}
		yield(rest)
	}
}

func renderAssistant(out *strings.Builder, e event.AssistantMessage) {
	if e.Reasoning != "" || e.ReasoningBlob != nil {
		writeElement(out, "<reasoning>", e.Reasoning, e.ReasoningBlob, "</reasoning>")
	}
	if e.Text != "" || e.TextBlob != nil {
		writeElement(out, "<assistant>", e.Text, e.TextBlob, "</assistant>")
	}
	for _, call := range e.ToolCalls {
		out.WriteString(`<tool_call name="`)
		out.WriteString(html.EscapeString(call.Name))
		out.WriteString(`" id="`)
		out.WriteString(html.EscapeString(call.ID))
		out.WriteString("\">\n")
		writeTextBytes(out, call.Args)
		writeBlob(out, call.ArgsBlob)
		out.WriteString("\n</tool_call>\n")
	}
}

func renderToolResult(out *strings.Builder, e event.ToolCompleted) {
	out.WriteString(`<tool_result id="`)
	out.WriteString(html.EscapeString(e.CallID))
	out.WriteString(`" ok="`)
	out.WriteString(strconv.FormatBool(e.OK))
	if e.ExitCode != nil {
		out.WriteString(`" exit="`)
		out.WriteString(strconv.Itoa(*e.ExitCode))
	}
	out.WriteString("\">\n")
	writeText(out, e.Output)
	out.WriteString("\n</tool_result>\n")
}

func renderTurnEnd(out *strings.Builder, e event.TurnCompleted) {
	if e.Status == "completed" {
		return
	}
	out.WriteString(`<turn_end status="`)
	out.WriteString(html.EscapeString(e.Status))
	out.WriteString(`">`)
	writeText(out, e.Error)
	out.WriteString("</turn_end>\n")
}

func writeElement(out *strings.Builder, open, text string, blob *event.BlobRef, closing string) {
	out.WriteString(open)
	out.WriteByte('\n')
	writeText(out, text)
	writeBlob(out, blob)
	out.WriteByte('\n')
	out.WriteString(closing)
	out.WriteByte('\n')
}

func writeBlob(out *strings.Builder, blob *event.BlobRef) {
	if blob == nil {
		return
	}
	out.WriteByte('[')
	out.WriteString(strconv.FormatInt(blob.Bytes, 10))
	out.WriteString(" bytes in blob ")
	writeText(out, blob.Blob)
	out.WriteByte(']')
}

func writeText(out *strings.Builder, text string) {
	for {
		index := strings.IndexAny(text, escapedBytes)
		if index < 0 {
			out.WriteString(text)
			return
		}
		out.WriteString(text[:index])
		out.WriteString(entity(text[index]))
		text = text[index+1:]
	}
}

func writeTextBytes(out *strings.Builder, text []byte) {
	for {
		index := bytes.IndexAny(text, escapedBytes)
		if index < 0 {
			out.Write(text)
			return
		}
		out.Write(text[:index])
		out.WriteString(entity(text[index]))
		text = text[index+1:]
	}
}

func entity(char byte) string {
	switch char {
	case '&':
		return "&amp;"
	case '<':
		return "&lt;"
	}
	return "&gt;"
}
