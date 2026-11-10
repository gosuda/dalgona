package auth

import (
	"fmt"
	"strings"
)

func checkHomeSDDL(dir, sddl, user string) error {
	entries, protected, sectionErr := homeDACLSection(dir, sddl)
	if sectionErr != nil {
		return sectionErr
	}
	if !protected {
		return fmt.Errorf("auth: home %s DACL inherits from its parent", dir)
	}
	if len(entries) == 0 {
		return fmt.Errorf("auth: home %s has an empty DACL", dir)
	}
	for _, entry := range entries {
		aceType, principal, ok := aceFields(entry)
		if !ok {
			return fmt.Errorf("auth: home %s has malformed ACL", dir)
		}
		allowed, known := homeACEAllowed(aceType, principal, user)
		if !known {
			return fmt.Errorf("auth: home %s has unknown ACE type %s", dir, aceType)
		}
		if !allowed {
			return fmt.Errorf("auth: home %s grants access to %s", dir, canonicalPrincipalSID(principal))
		}
	}
	return nil
}

func aceFields(entry string) (string, string, bool) {
	parts := strings.SplitN(entry, ";", 7)
	if len(parts) < 6 {
		return "", "", false
	}
	return parts[0], parts[5], true
}

func homeACEAllowed(aceType, principal, user string) (bool, bool) {
	switch aceType {
	case "A", "OA", "XA", "ZA":
		return homePrincipalAllowed(principal, user), true
	case "D", "OD", "XD", "AU", "AL", "OU", "OL", "ML", "RA", "SP", "XU", "TL", "FL":
		return true, true
	default:
		return false, false
	}
}

func homeDACLSection(dir, sddl string) ([]string, bool, error) {
	_, tail, found := strings.Cut(sddl, "D:")
	if !found {
		return nil, false, fmt.Errorf("auth: home %s has no DACL and grants access to everyone", dir)
	}
	if strings.Contains(tail, "NO_ACCESS_CONTROL") {
		return nil, false, fmt.Errorf("auth: home %s has no DACL and grants access to everyone", dir)
	}
	flags, rest, found := strings.Cut(tail, "(")
	if !found {
		return nil, false, fmt.Errorf("auth: home %s has an empty DACL", dir)
	}
	protected := strings.Contains(flags, "P")
	body := "(" + rest
	entries, ok := splitACEEntries(body[:daclSectionEnd(body)])
	if !ok {
		return nil, false, fmt.Errorf("auth: home %s has malformed ACL", dir)
	}
	return entries, protected, nil
}

func daclSectionEnd(body string) int {
	depth := 0
	for index := range len(body) {
		switch body[index] {
		case '(':
			depth++
		case ')':
			depth--
		default:
			if depth == 0 && body[index] == 'S' && strings.HasPrefix(body[index:], "S:") {
				return index
			}
		}
	}
	return len(body)
}

func splitACEEntries(body string) ([]string, bool) {
	var entries []string
	depth := 0
	start := 0
	for index := range len(body) {
		switch body[index] {
		case '(':
			if depth == 0 {
				start = index + 1
			}
			depth++
		case ')':
			depth--
			if depth < 0 {
				return nil, false
			}
			if depth == 0 {
				entries = append(entries, body[start:index])
			}
		}
	}
	if depth != 0 {
		return nil, false
	}
	return entries, true
}

func canonicalPrincipalSID(principal string) string {
	switch principal {
	case "SY":
		return "S-1-5-18"
	case "BA":
		return "S-1-5-32-544"
	case "WD":
		return "S-1-1-0"
	default:
		return principal
	}
}

func homePrincipalAllowed(principal, user string) bool {
	switch canonicalPrincipalSID(principal) {
	case user, "S-1-5-18", "S-1-5-32-544":
		return true
	default:
		return false
	}
}
