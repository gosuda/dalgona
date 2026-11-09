package stdlib

import (
	"errors"
	"fmt"
	"regexp"
	"strings"
	"unicode/utf8"

	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"
)

type matchMode uint8

const (
	searchMode matchMode = iota
	anchoredMatchMode
	fullMatchMode
)

func regexpModule() starlark.Value {
	return &starlarkstruct.Module{Name: "re", Members: starlark.StringDict{
		"compile":   starlark.NewBuiltin("re.compile", compilePattern),
		"search":    topLevelMatch("search", searchMode),
		"match":     topLevelMatch("match", anchoredMatchMode),
		"fullmatch": topLevelMatch("fullmatch", fullMatchMode),
		"findall":   topLevelFindall(),
		"sub":       topLevelSub(),
		"split":     topLevelSplit(),
	}}
}

func compilePattern(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var expression string
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 1, &expression); err != nil {
		return nil, err
	}
	return newPattern(expression)
}

func newPattern(expression string) (*pattern, error) {
	compiled, err := regexp.Compile(expression)
	if err != nil {
		return nil, fmt.Errorf("re: unsupported syntax: %w", err)
	}
	full, err := regexp.Compile(`\A(?:` + expression + `)\z`)
	if err != nil {
		return nil, fmt.Errorf("re: unsupported syntax: %w", err)
	}
	after, err := regexp.Compile(`\A(?s:.)(?:` + expression + `)`)
	if err != nil {
		return nil, fmt.Errorf("re: unsupported syntax: %w", err)
	}
	return &pattern{r: compiled, full: full, after: after}, nil
}

func topLevelMatch(name string, mode matchMode) *starlark.Builtin {
	return starlark.NewBuiltin("re."+name, func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		compiled, text, err := unpackPatternText(b.Name(), args, kwargs)
		if err != nil {
			return nil, err
		}
		return findMatch(compiled, text, mode), nil
	})
}

func unpackPatternText(name string, args starlark.Tuple, kwargs []starlark.Tuple) (*pattern, string, error) {
	var expression, text string
	if err := starlark.UnpackPositionalArgs(name, args, kwargs, 2, &expression, &text); err != nil {
		return nil, "", err
	}
	compiled, err := newPattern(expression)
	if err != nil {
		return nil, "", err
	}
	return compiled, text, nil
}

func findMatch(compiled *pattern, text string, mode matchMode) starlark.Value {
	selected := compiled.r
	if mode == fullMatchMode {
		selected = compiled.full
	}
	indices := selected.FindStringSubmatchIndex(text)
	if indices == nil || mode == anchoredMatchMode && indices[0] != 0 {
		return starlark.None
	}
	return &match{text: text, indices: indices, names: selected.SubexpNames()}
}

func topLevelFindall() *starlark.Builtin {
	return starlark.NewBuiltin("re.findall", func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		compiled, text, err := unpackPatternText(b.Name(), args, kwargs)
		if err != nil {
			return nil, err
		}
		return findAll(compiled, text), nil
	})
}

func findAll(compiled *pattern, text string) starlark.Value {
	matches := allMatches(compiled, text, -1)
	values := make([]starlark.Value, len(matches))
	for i, indices := range matches {
		values[i] = findAllValue(text, indices)
	}
	return starlark.NewList(values)
}

func findAllValue(text string, indices []int) starlark.Value {
	groups := len(indices)/2 - 1
	if groups == 0 {
		return starlark.String(text[indices[0]:indices[1]])
	}
	if groups == 1 {
		return starlark.String(matchedText(text, indices, 1))
	}
	values := make(starlark.Tuple, groups)
	for group := 1; group <= groups; group++ {
		values[group-1] = starlark.String(matchedText(text, indices, group))
	}
	return values
}

func matchedText(text string, indices []int, group int) string {
	start, end := indices[group*2], indices[group*2+1]
	if start < 0 {
		return ""
	}
	return text[start:end]
}

func topLevelSub() *starlark.Builtin {
	return starlark.NewBuiltin("re.sub", func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var expression, replacement, text string
		var count int
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "pattern", &expression, "repl", &replacement, "string", &text, "count?", &count); err != nil {
			return nil, err
		}
		compiled, err := newPattern(expression)
		if err != nil {
			return nil, err
		}
		return substitute(compiled, replacement, text, count)
	})
}

func substitute(compiled *pattern, replacement, text string, count int) (starlark.Value, error) {
	template, err := parseReplacement(compiled.r, replacement)
	if err != nil {
		return nil, err
	}
	limit := matchLimit(count)
	matches := allMatches(compiled, text, limit)
	var out strings.Builder
	out.Grow(len(text))
	last := 0
	for _, indices := range matches {
		out.WriteString(text[last:indices[0]])
		template.expand(&out, text, indices)
		last = indices[1]
	}
	out.WriteString(text[last:])
	return starlark.String(out.String()), nil
}

func matchLimit(count int) int {
	if count == 0 {
		return -1
	}
	return count
}

func topLevelSplit() *starlark.Builtin {
	return starlark.NewBuiltin("re.split", func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var expression, text string
		var maxSplit int
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "pattern", &expression, "string", &text, "maxsplit?", &maxSplit); err != nil {
			return nil, err
		}
		compiled, err := newPattern(expression)
		if err != nil {
			return nil, err
		}
		return split(compiled, text, maxSplit), nil
	})
}

func split(compiled *pattern, text string, maxSplit int) starlark.Value {
	matches := allMatches(compiled, text, matchLimit(maxSplit))
	values := make([]starlark.Value, 0, len(matches)+1)
	last := 0
	for _, indices := range matches {
		values = append(values, starlark.String(text[last:indices[0]]))
		for group := 1; group < len(indices)/2; group++ {
			values = append(values, groupValue(text, indices, group, starlark.None))
		}
		last = indices[1]
	}
	values = append(values, starlark.String(text[last:]))
	return starlark.NewList(values)
}

func allMatches(compiled *pattern, text string, limit int) [][]int {
	found := compiled.r.FindAllStringSubmatchIndex(text, limit)
	matches := make([][]int, 0, len(found)*2)
	for i, indices := range found {
		if limit >= 0 && len(matches) >= limit {
			break
		}
		matches = append(matches, indices)
		if limit >= 0 && len(matches) >= limit {
			break
		}
		if empty := missingEmptyMatch(compiled, text, found, i); empty != nil {
			matches = append(matches, empty)
		}
	}
	return matches
}

func missingEmptyMatch(compiled *pattern, text string, found [][]int, index int) []int {
	indices := found[index]
	if indices[0] == indices[1] {
		return nil
	}
	end := indices[1]
	if index+1 < len(found) && found[index+1][0] == end {
		return nil
	}
	_, width := utf8.DecodeLastRuneInString(text[:end])
	start := end - width
	empty := compiled.after.FindStringSubmatchIndex(text[start:])
	if empty == nil || empty[1] != width {
		return nil
	}
	for i := range empty {
		if empty[i] >= 0 {
			empty[i] += start
		}
	}
	empty[0] = empty[1]
	return empty
}

type pattern struct {
	r     *regexp.Regexp
	full  *regexp.Regexp
	after *regexp.Regexp
}

func (*pattern) String() string        { return "<re.Pattern>" }
func (*pattern) Type() string          { return "re.Pattern" }
func (*pattern) Freeze()               {}
func (*pattern) Truth() starlark.Bool  { return true }
func (*pattern) Hash() (uint32, error) { return 0, errors.New("unhashable type: re.Pattern") }

func (p *pattern) Attr(name string) (starlark.Value, error) {
	switch name {
	case "search":
		return boundMatch(p, name, searchMode), nil
	case "match":
		return boundMatch(p, name, anchoredMatchMode), nil
	case "fullmatch":
		return boundMatch(p, name, fullMatchMode), nil
	case "findall":
		return boundFindall(p), nil
	case "sub":
		return boundSub(p), nil
	case "split":
		return boundSplit(p), nil
	default:
		return nil, nil
	}
}

func (*pattern) AttrNames() []string {
	return []string{"findall", "fullmatch", "match", "search", "split", "sub"}
}

func boundMatch(p *pattern, name string, mode matchMode) *starlark.Builtin {
	return starlark.NewBuiltin("re.Pattern."+name, func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var text string
		if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 1, &text); err != nil {
			return nil, err
		}
		return findMatch(p, text, mode), nil
	})
}

func boundFindall(p *pattern) *starlark.Builtin {
	return starlark.NewBuiltin("re.Pattern.findall", func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var text string
		if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 1, &text); err != nil {
			return nil, err
		}
		return findAll(p, text), nil
	})
}

func boundSub(p *pattern) *starlark.Builtin {
	return starlark.NewBuiltin("re.Pattern.sub", func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var replacement, text string
		var count int
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "repl", &replacement, "string", &text, "count?", &count); err != nil {
			return nil, err
		}
		return substitute(p, replacement, text, count)
	})
}

func boundSplit(p *pattern) *starlark.Builtin {
	return starlark.NewBuiltin("re.Pattern.split", func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var text string
		var maxSplit int
		if err := starlark.UnpackArgs(b.Name(), args, kwargs, "string", &text, "maxsplit?", &maxSplit); err != nil {
			return nil, err
		}
		return split(p, text, maxSplit), nil
	})
}
