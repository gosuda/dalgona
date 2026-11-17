package runner

import (
	"errors"
	"fmt"
	"math"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/shellargs"
)

const guardMaxDepth = 8

var ErrProcessKillGuard = errors.New("runner: process-kill guard: cannot signal protected process")

type killGuard struct {
	daemonPID int
	parentPID int
	labels    func(pid int) []string
}

type killArgs struct {
	operands  []string
	signalled bool
	listing   bool
}

type patternArgs struct {
	patterns []string
	listing  bool
	inverse  bool
	regex    bool
}

func CheckProcessKillGuard(argv []string, daemonPID int, parentPID int) error {
	guard := killGuard{daemonPID: daemonPID, parentPID: parentPID, labels: processLabels}
	if guard.refusesTokens(argv, 0) {
		return fmt.Errorf("%w: cannot signal protected process", ErrProcessKillGuard)
	}
	return nil
}

func (g killGuard) refusesTokens(tokens []string, depth int) bool {
	return g.inspect(tokens, depth, false)
}

func (g killGuard) refusesFedTokens(tokens []string, depth int) bool {
	return g.inspect(tokens, depth, true)
}

func (g killGuard) inspect(tokens []string, depth int, fed bool) bool {
	if depth > guardMaxDepth {
		return true
	}
	tokens = shellargs.StripWrappers(tokens)
	if len(tokens) == 0 {
		return false
	}
	if shellargs.IsWrapper(tokens[0]) {
		return slices.ContainsFunc(tokens, containsKill)
	}
	name, args := path.Base(tokens[0]), tokens[1:]
	switch name {
	case "kill":
		return fed || g.refusesKill(args)
	case "pkill", "killall":
		return fed || g.refusesPatternKill(name, args)
	}
	return g.refusesNested(name, args, depth)
}

func (g killGuard) refusesNested(name string, args []string, depth int) bool {
	switch name {
	case "eval":
		return g.refusesCommand(strings.Join(args, " "), depth+1)
	case "xargs":
		return g.refusesFedTokens(xargsCommand(args), depth+1)
	case "find", ";", "+":
		return g.refusesFindActions(args, depth)
	}
	script, ok := shellScript(name, args)
	return ok && g.refusesCommand(script, depth+1)
}

func (g killGuard) refusesCommand(command string, depth int) bool {
	if depth > guardMaxDepth {
		return true
	}
	for _, body := range substitutions(command) {
		if g.refusesCommand(body, depth+1) {
			return true
		}
	}
	for _, part := range shellargs.Split(command) {
		if g.refusesPart(part, depth) {
			return true
		}
	}
	return false
}

func (g killGuard) refusesPart(part string, depth int) bool {
	tokens, ok := shellargs.Tokens(part)
	if !ok {
		return containsKill(part)
	}
	return g.refusesTokens(tokens, depth)
}

func (g killGuard) refusesFindActions(args []string, depth int) bool {
	for i, arg := range args {
		if isFindAction(arg) && g.refusesFedTokens(findActionTokens(args[i+1:]), depth+1) {
			return true
		}
	}
	return false
}

func (g killGuard) refusesKill(args []string) bool {
	parsed := parseKillArgs(args)
	return !parsed.listing && (len(parsed.operands) == 0 || slices.ContainsFunc(parsed.operands, g.protectedTarget))
}

func (g killGuard) protectedTarget(operand string) bool {
	pid, err := strconv.ParseInt(operand, 10, 64)
	if err != nil || pid <= 0 || pid > math.MaxInt32 {
		return true
	}
	return pid == int64(g.daemonPID) || pid == int64(g.parentPID)
}

func (g killGuard) refusesPatternKill(name string, args []string) bool {
	parsed := parsePatternArgs(name, args)
	if parsed.listing {
		return false
	}
	if parsed.inverse || len(parsed.patterns) == 0 || slices.Contains(parsed.patterns, "") {
		return true
	}
	selects := g.selectsName
	if name == "pkill" || parsed.regex {
		selects = g.selectsRegexp
	}
	return slices.ContainsFunc(parsed.patterns, selects)
}

func (g killGuard) selectsName(name string) bool {
	return slices.Contains(g.protectedLabels(), name)
}

func (g killGuard) selectsRegexp(pattern string) bool {
	matcher, err := regexp.Compile(pattern)
	if err != nil {
		return true
	}
	return slices.ContainsFunc(g.protectedLabels(), matcher.MatchString)
}

func (g killGuard) protectedLabels() []string {
	labels := []string{"dal", "dalgona"}
	if exe, err := os.Executable(); err == nil {
		labels = append(labels, filepath.Base(exe))
	}
	labels = append(labels, g.labels(g.daemonPID)...)
	return append(labels, g.labels(g.parentPID)...)
}

func parseKillArgs(args []string) killArgs {
	var parsed killArgs
	for i := 0; i < len(args); i++ {
		arg := args[i]
		switch {
		case arg == "--":
			parsed.operands = append(parsed.operands, args[i+1:]...)
			return parsed
		case slices.Contains([]string{"-l", "-L", "--list", "--table", "-h", "--help", "-V", "--version"}, arg):
			parsed.listing = true
		case slices.Contains([]string{"-s", "-n", "-q", "--signal", "--queue"}, arg):
			parsed.signalled = true
			i++
		case strings.HasPrefix(arg, "--signal="):
			parsed.signalled = true
		case !parsed.signalled && isSignalSpec(arg):
			parsed.signalled = true
		default:
			parsed.operands = append(parsed.operands, arg)
		}
	}
	return parsed
}

func isSignalSpec(arg string) bool {
	name, found := strings.CutPrefix(arg, "-")
	if !found || name == "" {
		return false
	}
	if isDecimal(name) {
		return true
	}
	return isASCIILetter(rune(name[0])) && !strings.ContainsFunc(name, isNotSignalRune)
}

func isNotSignalRune(r rune) bool {
	return r != '+' && r != '-' && !isASCIILetter(r) && (r < '0' || r > '9')
}

func isASCIILetter(r rune) bool {
	return r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z'
}

func isDecimal(text string) bool {
	return text != "" && !strings.ContainsFunc(text, func(r rune) bool { return r < '0' || r > '9' })
}

func parsePatternArgs(name string, args []string) patternArgs {
	var parsed patternArgs
	for i := 0; i < len(args); i++ {
		arg := args[i]
		switch {
		case arg == "--":
			parsed.patterns = append(parsed.patterns, args[i+1:]...)
			return parsed
		case isPatternValued(name, arg):
			i++
		case !strings.HasPrefix(arg, "-") || arg == "-":
			parsed.patterns = append(parsed.patterns, arg)
		default:
			parsed.note(name, arg)
		}
	}
	return parsed
}

func (p *patternArgs) note(name, arg string) {
	switch {
	case isInfoOption(name, arg):
		p.listing = true
	case isInverseOption(name, arg):
		p.inverse = true
	case isRegexOption(name, arg):
		p.regex = true
	}
}

func isPatternValued(name, arg string) bool {
	valued := []string{"-u", "-U", "-g", "-G", "-P", "-s", "-t", "-T", "-c", "-F", "--euid", "--uid", "--pgroup", "--group", "--parent", "--session", "--terminal", "--ns", "--nslist", "--pidfile", "--signal"}
	if name == "killall" {
		valued = []string{"-s", "-u", "-y", "-o", "-n", "-Z", "--signal", "--user", "--younger-than", "--older-than", "--ns", "--context"}
	}
	return slices.Contains(valued, arg)
}

func isInfoOption(name, arg string) bool {
	return slices.Contains([]string{"-V", "--version", "-h", "--help"}, arg) || name == "killall" && slices.Contains([]string{"-l", "--list"}, arg)
}

func isInverseOption(name, arg string) bool {
	return name == "pkill" && (arg == "--inverse" || isShortCluster(arg) && strings.Contains(arg, "v"))
}

func isRegexOption(name, arg string) bool {
	return name == "killall" && (arg == "--regexp" || isShortCluster(arg) && strings.Contains(arg, "r"))
}

func isShortCluster(arg string) bool {
	return strings.HasPrefix(arg, "-") && !strings.HasPrefix(arg, "--")
}

func xargsCommand(args []string) []string {
	for len(args) > 0 {
		arg := args[0]
		switch {
		case arg == "--":
			return args[1:]
		case !strings.HasPrefix(arg, "-") || arg == "-":
			return args
		case isXargsValued(arg):
			args = args[min(2, len(args)):]
		default:
			args = args[1:]
		}
	}
	return args
}

func isXargsValued(arg string) bool {
	return slices.Contains([]string{"-a", "-d", "-E", "-I", "-L", "-n", "-P", "-s", "--arg-file", "--delimiter", "--eof", "--max-lines", "--max-args", "--max-procs", "--max-chars"}, arg)
}

func isFindAction(arg string) bool {
	return slices.Contains([]string{"-exec", "-execdir", "-ok", "-okdir"}, arg)
}

func findActionTokens(args []string) []string {
	end := slices.IndexFunc(args, func(arg string) bool { return arg == ";" || arg == "+" })
	if end < 0 {
		return args
	}
	return args[:end]
}

func shellScript(name string, args []string) (string, bool) {
	if !slices.Contains([]string{"sh", "bash", "zsh", "dash", "ksh", "ash", "mksh", "fish"}, name) {
		return "", false
	}
	for i := 0; i < len(args); i++ {
		arg := args[i]
		switch {
		case slices.Contains([]string{"-o", "+o", "-O", "+O", "--rcfile", "--init-file"}, arg):
			i++
		case isShortCluster(arg) && strings.Contains(arg, "c"):
			if i+1 < len(args) {
				return args[i+1], true
			}
			return "", false
		case !isShellOption(arg):
			return "", false
		}
	}
	return "", false
}

func isShellOption(arg string) bool {
	return len(arg) > 1 && (arg[0] == '-' || arg[0] == '+')
}

func containsKill(text string) bool {
	return strings.Contains(text, "kill")
}

func substitutions(command string) []string {
	var bodies []string
	inDouble := false
	for i := 0; i < len(command); i++ {
		switch c := command[i]; {
		case c == '\\':
			i++
		case c == '"':
			inDouble = !inDouble
		case c == '\'' && !inDouble:
			i = indexFrom(command, '\'', i+1)
		case c == '`':
			end := indexFrom(command, '`', i+1)
			bodies = append(bodies, command[i+1:end])
			i = end
		case c == '$' && strings.HasPrefix(command[i:], "$("):
			end := parenEnd(command, i+2)
			bodies = append(bodies, command[i+2:end])
			i = end
		}
	}
	return bodies
}

func indexFrom(text string, target byte, start int) int {
	if start >= len(text) {
		return len(text)
	}
	if end := strings.IndexByte(text[start:], target); end >= 0 {
		return start + end
	}
	return len(text)
}

func parenEnd(command string, start int) int {
	depth := 1
	for i := start; i < len(command); i++ {
		switch command[i] {
		case '\\':
			i++
		case '\'', '"':
			i = indexFrom(command, command[i], i+1)
		case '(':
			depth++
		case ')':
			depth--
			if depth == 0 {
				return i
			}
		}
	}
	return len(command)
}
