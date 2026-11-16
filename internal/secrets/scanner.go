package secrets

import (
	"cmp"
	"regexp"
	"slices"
	"strings"
)

const openAIKeyType = "openai_key"

type Hit struct {
	Rule  int
	Type  string
	Start int
	End   int
}

type rule struct {
	typ     string
	needles []string
	re      *regexp.Regexp
}

func newRule(typ, expression string, needles ...string) rule {
	return rule{typ: typ, needles: needles, re: regexp.MustCompile(expression)}
}

var rules = []rule{
	newRule("aws_access_key_id", `\bAKIA[0-9A-Z]{16}`, "AKIA"),
	newRule("github_token", `\bgh[pousr]_[A-Za-z0-9]{36,}`, "ghp_", "gho_", "ghu_", "ghs_", "ghr_"),
	newRule("github_pat", `\bgithub_pat_[A-Za-z0-9_]{22,}`, "github_pat_"),
	newRule("slack_token", `\bxox[baprs]-[A-Za-z0-9-]{10,}`, "xox"),
	newRule("google_api_key", `\bAIza[0-9A-Za-z\-_]{35,}`, "AIza"),
	newRule("anthropic_key", `\bsk-ant-(?:api\d{2}-)?[A-Za-z0-9_-]{20,}`, "sk-ant-"),
	newRule(openAIKeyType, `\bsk-[A-Za-z0-9_-]{20,}`, "sk-"),
	newRule("private_key_block", `(?s)-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----.*?-----END (?:[A-Z0-9]+ )*PRIVATE KEY-----`, "-----BEGIN "),
	newRule("jwt", `\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}(?:\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,})?`, "eyJ"),
}

func Scan(text string) []Hit {
	var hits []Hit
	for index, candidate := range rules {
		if candidate.appearsIn(text) {
			hits = candidate.appendHits(hits, index, text)
		}
	}
	return disjoint(hits)
}

func (r rule) appearsIn(text string) bool {
	return slices.ContainsFunc(r.needles, func(needle string) bool { return strings.Contains(text, needle) })
}

func (r rule) appendHits(hits []Hit, index int, text string) []Hit {
	for _, location := range r.re.FindAllStringIndex(text, -1) {
		if r.typ == openAIKeyType && kebabPhrase(text[location[0]:location[1]]) {
			continue
		}
		hits = append(hits, Hit{Rule: index, Type: r.typ, Start: location[0], End: location[1]})
	}
	return hits
}

func disjoint(hits []Hit) []Hit {
	slices.SortFunc(hits, compareHits)
	kept := make([]Hit, 0, len(hits))
	end := 0
	for _, hit := range hits {
		if hit.Start < end {
			continue
		}
		kept = append(kept, hit)
		end = hit.End
	}
	return kept
}

func compareHits(a, b Hit) int {
	return cmp.Or(
		cmp.Compare(a.Start, b.Start),
		cmp.Compare(b.End-b.Start, a.End-a.Start),
		cmp.Compare(a.Rule, b.Rule),
	)
}

func kebabPhrase(match string) bool {
	return !knownOpenAIKeyPrefix(match) && !strings.ContainsAny(match, "0123456789") &&
		strings.Contains(strings.TrimPrefix(match, "sk-"), "-")
}

func knownOpenAIKeyPrefix(match string) bool {
	return strings.HasPrefix(match, "sk-proj-") ||
		strings.HasPrefix(match, "sk-svcacct-") ||
		strings.HasPrefix(match, "sk-admin-")
}
