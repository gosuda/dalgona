package shellargs

import (
	"path"
	"slices"
	"strings"
)

type grammar struct {
	flags    string
	valued   string
	long     []string
	longEq   []string
	numeric  bool
	duration bool
}

func IsWrapper(token string) bool {
	_, ok := grammarOf(wrapperName(token))
	return ok
}

func StripWrappers(tokens []string) []string {
	for len(tokens) > 0 {
		rest, ok := stripHead(tokens)
		if !ok {
			return tokens
		}
		tokens = rest
	}
	return tokens
}

func stripHead(tokens []string) ([]string, bool) {
	head := tokens[0]
	if isAssignment(head) || isControlKeyword(head) {
		return tokens[1:], true
	}
	spec, ok := grammarOf(wrapperName(head))
	if !ok {
		return tokens, false
	}
	return spec.skip(tokens[1:])
}

func wrapperName(token string) string {
	return path.Base(strings.TrimSpace(token))
}

func grammarOf(name string) (grammar, bool) {
	switch name {
	case "env":
		return grammar{flags: "i0", valued: "uC", long: []string{"--ignore-environment", "--null"}, longEq: []string{"--unset", "--chdir"}}, true
	case "command", "time":
		return grammar{flags: "p"}, true
	case "builtin", "nohup", "watch", "npx", "bunx", "pnpx":
		return grammar{}, true
	case "exec":
		return grammar{flags: "cl", valued: "a"}, true
	case "nice":
		return grammar{valued: "n", longEq: []string{"--adjustment"}, numeric: true}, true
	case "timeout":
		return grammar{flags: "v", valued: "sk", long: []string{"--foreground", "--preserve-status", "--verbose"}, longEq: []string{"--signal", "--kill-after"}, duration: true}, true
	case "sudo", "doas":
		return grammar{flags: "En", valued: "u"}, true
	}
	return grammar{}, false
}

func (g grammar) skip(args []string) ([]string, bool) {
	args, ok := g.skipOptions(args)
	if !ok || !g.duration {
		return args, ok
	}
	if len(args) == 0 || !isDuration(args[0]) {
		return nil, false
	}
	return args[1:], true
}

func (g grammar) skipOptions(args []string) ([]string, bool) {
	for len(args) > 0 {
		arg := args[0]
		if arg == "--" {
			return args[1:], true
		}
		if !strings.HasPrefix(arg, "-") || arg == "-" {
			return args, true
		}
		count, ok := g.optionLength(arg)
		if !ok || count > len(args) {
			return nil, false
		}
		args = args[count:]
	}
	return args, true
}

func (g grammar) optionLength(arg string) (int, bool) {
	if strings.HasPrefix(arg, "--") {
		return g.longLength(arg)
	}
	return g.shortLength(arg[1:])
}

func (g grammar) longLength(arg string) (int, bool) {
	name, _, hasValue := strings.Cut(arg, "=")
	if hasValue && slices.Contains(g.longEq, name) || !hasValue && slices.Contains(g.long, name) {
		return 1, true
	}
	return 0, false
}

func (g grammar) shortLength(chars string) (int, bool) {
	if g.numeric && isDigits(chars) {
		return 1, true
	}
	for i := range len(chars) {
		switch {
		case strings.IndexByte(g.flags, chars[i]) >= 0:
		case strings.IndexByte(g.valued, chars[i]) >= 0:
			if i < len(chars)-1 {
				return 1, true
			}
			return 2, true
		default:
			return 0, false
		}
	}
	return 1, true
}

func isAssignment(token string) bool {
	name, _, found := strings.Cut(token, "=")
	if !found || name == "" || isDigits(name[:1]) {
		return false
	}
	return !strings.ContainsFunc(name, func(r rune) bool { return !isNameChar(r) })
}

func isNameChar(r rune) bool {
	return r == '_' || r >= '0' && r <= '9' || r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z'
}

func isControlKeyword(token string) bool {
	switch strings.TrimSpace(token) {
	case "if", "then", "elif", "else", "fi", "while", "until", "do", "done", "for", "in", "case", "esac", "!":
		return true
	}
	return false
}

func isDigits(text string) bool {
	return text != "" && !strings.ContainsFunc(text, func(r rune) bool { return r < '0' || r > '9' })
}

func isDuration(text string) bool {
	if text != "" && strings.IndexByte("smhd", text[len(text)-1]) >= 0 {
		text = text[:len(text)-1]
	}
	whole, fraction, hasDot := strings.Cut(text, ".")
	return isDigits(whole) && (!hasDot || isDigits(fraction))
}
