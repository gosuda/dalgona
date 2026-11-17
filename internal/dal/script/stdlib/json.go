package stdlib

import (
	"bytes"
	"errors"
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"
	"unicode/utf16"
	"unicode/utf8"

	starlarkjson "go.starlark.net/lib/json"
	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"
)

type indentOption struct {
	text   string
	pretty bool
}

func (o *indentOption) Unpack(value starlark.Value) error {
	switch value := value.(type) {
	case starlark.NoneType:
		*o = indentOption{}
		return nil
	case starlark.String:
		o.text = string(value)
		o.pretty = true
		return nil
	case starlark.Int:
		var count int
		if err := starlark.AsInt(value, &count); err != nil {
			return err
		}
		if count < 0 {
			count = 0
		}
		o.text = strings.Repeat(" ", count)
		o.pretty = true
		return nil
	default:
		return fmt.Errorf("got %s, want int, string, or None", value.Type())
	}
}

func jsonModule() starlark.Value {
	return &starlarkstruct.Module{Name: "json", Members: starlark.StringDict{
		"dumps": starlark.NewBuiltin("json.dumps", jsonDumps),
		"loads": starlarkjson.Module.Members["decode"],
	}}
}

func jsonDumps(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var value starlark.Value
	var indent indentOption
	var sortKeys bool
	if err := starlark.UnpackArgs(b.Name(), args, kwargs, "obj", &value, "indent?", &indent, "sort_keys?", &sortKeys); err != nil {
		return nil, err
	}
	encoder := jsonEncoder{indent: indent.text, pretty: indent.pretty, sortKeys: sortKeys, seen: map[starlark.Value]bool{}}
	if err := encoder.encode(value); err != nil {
		return nil, fmt.Errorf("json.dumps: %w", err)
	}
	return starlark.String(encoder.buf.String()), nil
}

type jsonEncoder struct {
	buf      bytes.Buffer
	indent   string
	pretty   bool
	sortKeys bool
	seen     map[starlark.Value]bool
	depth    int
}

func (e *jsonEncoder) encode(value starlark.Value) error {
	switch value := value.(type) {
	case starlark.NoneType:
		e.buf.WriteString("null")
	case starlark.Bool:
		e.buf.WriteString(strconv.FormatBool(bool(value)))
	case starlark.Int:
		e.buf.WriteString(value.String())
	case starlark.Float:
		e.buf.WriteString(pythonFloat(float64(value)))
	case starlark.String:
		e.quote(string(value))
	default:
		return e.encodeCompound(value)
	}
	return nil
}

func (e *jsonEncoder) encodeCompound(value starlark.Value) error {
	switch value := value.(type) {
	case *starlark.Dict:
		return e.encodeDict(value)
	case *starlark.List:
		return e.encodeList(value)
	case starlark.Tuple:
		return e.encodeIterable(value)
	case starlark.Iterable:
		return e.encodeIterable(value)
	case starlark.HasAttrs:
		return e.encodeAttrs(value)
	default:
		return fmt.Errorf("cannot encode %s as JSON", value.Type())
	}
}

func (e *jsonEncoder) enter(value starlark.Value) error {
	if e.seen[value] {
		return errors.New("cycle in JSON structure")
	}
	e.seen[value] = true
	return nil
}

func (e *jsonEncoder) encodeDict(dict *starlark.Dict) error {
	if err := e.enter(dict); err != nil {
		return err
	}
	defer delete(e.seen, dict)
	items := dict.Items()
	for _, item := range items {
		if _, ok := item[0].(starlark.String); !ok {
			return fmt.Errorf("dict has %s key, want string", item[0].Type())
		}
	}
	if e.sortKeys {
		slices.SortFunc(items, func(a, b starlark.Tuple) int {
			return strings.Compare(string(a[0].(starlark.String)), string(b[0].(starlark.String)))
		})
	}
	e.startCompound('{', len(items))
	e.depth++
	for i, item := range items {
		e.beforeElement(i, e.depth)
		e.quote(string(item[0].(starlark.String)))
		e.buf.WriteString(": ")
		if err := e.encode(item[1]); err != nil {
			return err
		}
	}
	e.depth--
	e.endCompound('}', len(items), e.depth)
	return nil
}

func (e *jsonEncoder) encodeList(list *starlark.List) error {
	if err := e.enter(list); err != nil {
		return err
	}
	defer delete(e.seen, list)
	return e.encodeElements(list)
}

func (e *jsonEncoder) encodeIterable(iterable starlark.Iterable) error {
	values := make([]starlark.Value, 0)
	iterator := iterable.Iterate()
	defer iterator.Done()
	var value starlark.Value
	for iterator.Next(&value) {
		values = append(values, value)
	}
	return e.encodeElements(starlark.NewList(values))
}

func (e *jsonEncoder) encodeElements(list *starlark.List) error {
	e.startCompound('[', list.Len())
	e.depth++
	for i := range list.Len() {
		e.beforeElement(i, e.depth)
		if err := e.encode(list.Index(i)); err != nil {
			return err
		}
	}
	e.depth--
	e.endCompound(']', list.Len(), e.depth)
	return nil
}

func (e *jsonEncoder) encodeAttrs(value starlark.HasAttrs) error {
	names := slices.Clone(value.AttrNames())
	slices.Sort(names)
	e.startCompound('{', len(names))
	e.depth++
	for i, name := range names {
		attribute, err := value.Attr(name)
		if err != nil {
			return fmt.Errorf("access attribute %s: %w", name, err)
		}
		e.beforeElement(i, e.depth)
		e.quote(name)
		e.buf.WriteString(": ")
		if err := e.encode(attribute); err != nil {
			return err
		}
	}
	e.depth--
	e.endCompound('}', len(names), e.depth)
	return nil
}

func (e *jsonEncoder) startCompound(token byte, count int) {
	e.buf.WriteByte(token)
	if e.pretty && count > 0 {
		e.buf.WriteByte('\n')
	}
}

func (e *jsonEncoder) beforeElement(index, depth int) {
	if index > 0 {
		e.buf.WriteByte(',')
		if e.pretty {
			e.buf.WriteByte('\n')
		} else {
			e.buf.WriteByte(' ')
		}
	}
	if e.pretty {
		e.writeIndent(depth)
	}
}

func (e *jsonEncoder) writeIndent(depth int) {
	for range depth {
		e.buf.WriteString(e.indent)
	}
}

func (e *jsonEncoder) endCompound(token byte, count, depth int) {
	if e.pretty && count > 0 {
		e.buf.WriteByte('\n')
		e.writeIndent(depth)
	}
	e.buf.WriteByte(token)
}

func (e *jsonEncoder) quote(text string) {
	e.buf.WriteByte('"')
	for _, r := range text {
		e.writeRune(r)
	}
	e.buf.WriteByte('"')
}

func (e *jsonEncoder) writeRune(r rune) {
	switch r {
	case '"', '\\':
		e.buf.WriteByte('\\')
		e.buf.WriteRune(r)
	case '\b':
		e.buf.WriteString(`\b`)
	case '\f':
		e.buf.WriteString(`\f`)
	case '\n':
		e.buf.WriteString(`\n`)
	case '\r':
		e.buf.WriteString(`\r`)
	case '\t':
		e.buf.WriteString(`\t`)
	default:
		e.writeNonControlRune(r)
	}
}

func (e *jsonEncoder) writeNonControlRune(r rune) {
	if r >= 0x20 && r < utf8.RuneSelf {
		e.buf.WriteRune(r)
		return
	}
	if r <= 0xffff {
		e.writeUnicodeEscape(r)
		return
	}
	high, low := utf16.EncodeRune(r)
	e.writeUnicodeEscape(high)
	e.writeUnicodeEscape(low)
}

func (e *jsonEncoder) writeUnicodeEscape(value rune) {
	const hexadecimal = "0123456789abcdef"
	e.buf.WriteString(`\u`)
	e.buf.WriteByte(hexadecimal[value>>12])
	e.buf.WriteByte(hexadecimal[(value>>8)&0xf])
	e.buf.WriteByte(hexadecimal[(value>>4)&0xf])
	e.buf.WriteByte(hexadecimal[value&0xf])
}

func pythonFloat(value float64) string {
	if math.IsNaN(value) {
		return "NaN"
	}
	if math.IsInf(value, 1) {
		return "Infinity"
	}
	if math.IsInf(value, -1) {
		return "-Infinity"
	}
	scientific := strconv.FormatFloat(value, 'e', -1, 64)
	exponent, err := strconv.Atoi(scientific[strings.LastIndexByte(scientific, 'e')+1:])
	if err == nil && exponent >= -4 && exponent < 16 {
		fixed := strconv.FormatFloat(value, 'f', -1, 64)
		if !strings.ContainsRune(fixed, '.') {
			fixed += ".0"
		}
		return fixed
	}
	return scientific
}
