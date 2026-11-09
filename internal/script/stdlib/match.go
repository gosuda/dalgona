package stdlib

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"go.starlark.net/starlark"
)

type match struct {
	text    string
	indices []int
	names   []string
}

func (m *match) String() string {
	return fmt.Sprintf("<re.Match object; span=(%d, %d), match=%s>", m.offset(m.indices[0]), m.offset(m.indices[1]), pythonStringRepr(matchedText(m.text, m.indices, 0)))
}
func (*match) Type() string          { return "re.Match" }
func (*match) Freeze()               {}
func (*match) Truth() starlark.Bool  { return true }
func (*match) Hash() (uint32, error) { return 0, errors.New("unhashable type: re.Match") }

func (m *match) Attr(name string) (starlark.Value, error) {
	switch name {
	case "group":
		return starlark.NewBuiltin("re.Match.group", m.group), nil
	case "groups":
		return starlark.NewBuiltin("re.Match.groups", m.groups), nil
	case "start":
		return starlark.NewBuiltin("re.Match.start", m.start), nil
	case "end":
		return starlark.NewBuiltin("re.Match.end", m.end), nil
	case "span":
		return starlark.NewBuiltin("re.Match.span", m.span), nil
	default:
		return nil, nil
	}
}

func (*match) AttrNames() []string {
	return []string{"end", "group", "groups", "span", "start"}
}

func (m *match) group(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	if len(kwargs) != 0 {
		return nil, fmt.Errorf("%s: unexpected keyword arguments", b.Name())
	}
	if len(args) == 0 {
		return groupValue(m.text, m.indices, 0, starlark.None), nil
	}
	if len(args) == 1 {
		index, err := m.groupIndex(args[0])
		if err != nil {
			return nil, err
		}
		return groupValue(m.text, m.indices, index, starlark.None), nil
	}
	values := make(starlark.Tuple, len(args))
	for i, arg := range args {
		index, err := m.groupIndex(arg)
		if err != nil {
			return nil, err
		}
		values[i] = groupValue(m.text, m.indices, index, starlark.None)
	}
	return values, nil
}

func (m *match) groups(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var defaultValue starlark.Value = starlark.None
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 0, &defaultValue); err != nil {
		return nil, err
	}
	values := make(starlark.Tuple, len(m.indices)/2-1)
	for group := 1; group < len(m.indices)/2; group++ {
		values[group-1] = groupValue(m.text, m.indices, group, defaultValue)
	}
	return values, nil
}

func (m *match) start(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	index, err := m.unpackGroup(b.Name(), args, kwargs)
	if err != nil {
		return nil, err
	}
	return starlark.MakeInt(m.offset(m.indices[index*2])), nil
}

func (m *match) end(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	index, err := m.unpackGroup(b.Name(), args, kwargs)
	if err != nil {
		return nil, err
	}
	return starlark.MakeInt(m.offset(m.indices[index*2+1])), nil
}

func (m *match) span(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	index, err := m.unpackGroup(b.Name(), args, kwargs)
	if err != nil {
		return nil, err
	}
	return starlark.Tuple{starlark.MakeInt(m.offset(m.indices[index*2])), starlark.MakeInt(m.offset(m.indices[index*2+1]))}, nil
}

func (m *match) offset(byteOffset int) int {
	if byteOffset < 0 {
		return -1
	}
	return utf8.RuneCountInString(m.text[:byteOffset])
}

func (m *match) unpackGroup(name string, args starlark.Tuple, kwargs []starlark.Tuple) (int, error) {
	var value starlark.Value = starlark.MakeInt(0)
	if err := starlark.UnpackPositionalArgs(name, args, kwargs, 0, &value); err != nil {
		return 0, err
	}
	return m.groupIndex(value)
}

func (m *match) groupIndex(value starlark.Value) (int, error) {
	switch value := value.(type) {
	case starlark.Int:
		var index int
		if err := starlark.AsInt(value, &index); err != nil {
			return 0, err
		}
		return m.checkGroupIndex(index)
	case starlark.String:
		return m.namedGroupIndex(string(value))
	default:
		return 0, fmt.Errorf("got %s, want int or string group", value.Type())
	}
}

func (m *match) checkGroupIndex(index int) (int, error) {
	if index < 0 || index >= len(m.indices)/2 {
		return 0, fmt.Errorf("no such group: %d", index)
	}
	return index, nil
}

func (m *match) namedGroupIndex(name string) (int, error) {
	for index, candidate := range m.names {
		if index > 0 && candidate == name {
			return index, nil
		}
	}
	return 0, fmt.Errorf("no such group: %s", name)
}

func groupValue(text string, indices []int, group int, unmatched starlark.Value) starlark.Value {
	start, end := indices[group*2], indices[group*2+1]
	if start < 0 {
		return unmatched
	}
	return starlark.String(text[start:end])
}

func pythonStringRepr(text string) string {
	quote := "'"
	if strings.Contains(text, "'") && !strings.Contains(text, `"`) {
		quote = `"`
	}
	var out strings.Builder
	out.Grow(len(text) + 2)
	out.WriteString(quote)
	for _, r := range text {
		writePythonReprRune(&out, r, quote[0])
	}
	out.WriteString(quote)
	return out.String()
}

func writePythonReprRune(out *strings.Builder, r rune, quote byte) {
	switch {
	case r == '\\':
		out.WriteString(`\\`)
	case r == rune(quote):
		out.WriteByte('\\')
		out.WriteRune(r)
	case r == '\t':
		out.WriteString(`\t`)
	case r == '\n':
		out.WriteString(`\n`)
	case r == '\r':
		out.WriteString(`\r`)
	case unicode.IsPrint(r):
		out.WriteRune(r)
	default:
		writePythonCodeEscape(out, r)
	}
}

func writePythonCodeEscape(out *strings.Builder, r rune) {
	if r <= 0xff {
		writeHexEscape(out, 'x', r, 2)
		return
	}
	if r <= 0xffff {
		writeHexEscape(out, 'u', r, 4)
		return
	}
	writeHexEscape(out, 'U', r, 8)
}

func writeHexEscape(out *strings.Builder, prefix byte, r rune, width int) {
	digits := strconv.FormatInt(int64(r), 16)
	out.WriteByte('\\')
	out.WriteByte(prefix)
	for range width - len(digits) {
		out.WriteByte('0')
	}
	out.WriteString(digits)
}
