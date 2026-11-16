package rules

import (
	"regexp"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/shellargs"
)

type invocation struct {
	part       string
	tokens     []string
	normal     string
	opaque     bool
	privileged bool
}

type riskyRule struct {
	name  string
	match func(invocation) bool
}

var substitution = regexp.MustCompile(`\$\(|<\(|>\(|` + "`" + `|\\\r?\n`)

var risky = []riskyRule{
	{"shell-wrapper", func(c invocation) bool { return c.opaque }},
	{"nested-shell", pattern(`^(bash|sh|zsh|fish|eval|xargs)\b`)},
	{"inline-interpreter", pattern(`^(node|bun|python3?)\s+(-\S+\s+)*(-c|-e|-p|--eval)\b`)},
	{"recursive-delete", func(c invocation) bool { return isRecursiveRm(c.tokens) }},
	{"git-destructive", pattern(`^git\b.*\b(push|reset\s+--hard|clean\s+-\S*[fd]|branch\s+-D)\b`)},
	{"publish", pattern(`^(npm|pnpm|yarn|bun)\s+publish\b`)},
	{"deploy", pattern(`^(wrangler|vercel|fly|flyctl)\s+deploy\b|^terraform\s+(apply|destroy)\b|^kubectl\s+(delete|apply)\b`)},
	{"credentials", credentials()},
	{"privilege", privilege()},
}

func pattern(expr string) func(invocation) bool {
	re := regexp.MustCompile(expr)
	return func(c invocation) bool { return re.MatchString(c.normal) }
}

func credentials() func(invocation) bool {
	re := regexp.MustCompile(`(?i)\.env\b|\.ssh\b|\.aws\b|\.npmrc\b|\.netrc\b|credentials`)
	return func(c invocation) bool { return re.MatchString(c.normal) || re.MatchString(c.part) }
}

func privilege() func(invocation) bool {
	re := regexp.MustCompile(`^(sudo|doas|su)\b`)
	return func(c invocation) bool { return c.privileged || re.MatchString(c.normal) }
}

func inspect(part string) invocation {
	tokens, ok := shellargs.Tokens(part)
	if !ok {
		return invocation{part: part, normal: part, opaque: true}
	}
	stripped := shellargs.StripWrappers(tokens)
	if len(stripped) == 0 {
		return invocation{}
	}
	name := executableName(stripped[0])
	return invocation{
		part:       part,
		tokens:     stripped,
		normal:     joinCommand(name, stripped[1:]),
		opaque:     isOpaqueName(name, stripped[0]),
		privileged: slices.ContainsFunc(tokens[:len(tokens)-len(stripped)], isPrivilegeWrapper),
	}
}

func executableName(token string) string {
	name := strings.TrimSpace(token)
	if _, base, found := strings.CutLast(name, "/"); found {
		return base
	}
	return name
}

func joinCommand(name string, args []string) string {
	words := make([]string, 0, 1+len(args))
	words = append(words, shellargs.QuoteToken(name))
	for _, arg := range args {
		words = append(words, shellargs.QuoteToken(arg))
	}
	return strings.Join(words, " ")
}

func isOpaqueName(name, head string) bool {
	if name == "" || shellargs.IsWrapper(head) || strings.ContainsAny(name, "$=") {
		return true
	}
	return name[0] == '-' || name[0] >= '0' && name[0] <= '9'
}

func isPrivilegeWrapper(token string) bool {
	name := executableName(token)
	return name == "sudo" || name == "doas"
}

func isRecursiveRm(tokens []string) bool {
	if len(tokens) == 0 || executableName(tokens[0]) != "rm" {
		return false
	}
	for _, token := range tokens[1:] {
		if token == "--" {
			return false
		}
		if isRecursiveFlag(token) {
			return true
		}
	}
	return false
}

func isRecursiveFlag(token string) bool {
	if token == "--recursive" {
		return true
	}
	return len(token) > 1 && token[0] == '-' && token[1] != '-' && strings.ContainsAny(token[1:], "rR")
}

func Check(command string) (string, string, bool) {
	if substitution.MatchString(command) {
		return "substitution", command, true
	}
	return firstRisk(risky, command)
}

func CheckRemoteValue(value string) (string, string, bool) {
	return firstRisk(risky[1:], value)
}

func firstRisk(rulesList []riskyRule, command string) (string, string, bool) {
	for _, part := range shellargs.Split(command) {
		inv := inspect(part)
		for _, rule := range rulesList {
			if rule.match(inv) {
				return rule.name, part, true
			}
		}
	}
	return "", "", false
}

func GrantPrefix(command string) (string, bool) {
	parts := shellargs.Split(command)
	if len(parts) != 1 {
		return "", false
	}
	tokens, ok := shellargs.Tokens(parts[0])
	if !ok || len(tokens) == 0 {
		return "", false
	}
	prefix := shellargs.QuoteToken(tokens[0])
	if len(tokens) > 1 {
		prefix += " " + shellargs.QuoteToken(tokens[1])
	}
	return prefix, true
}

func MatchesGrant(command, prefix string) bool {
	parts := shellargs.Split(command)
	if len(parts) != 1 {
		return false
	}
	granted, grantedOK := shellargs.Tokens(prefix)
	candidate, candidateOK := shellargs.Tokens(parts[0])
	if !grantedOK || !candidateOK || len(granted) == 0 || len(candidate) < len(granted) {
		return false
	}
	return slices.Equal(candidate[:len(granted)], granted)
}
