package graph

import (
	"bytes"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"sort"
	"strings"
	"unicode"
	"unicode/utf8"
)

const (
	maxNodes       = 64
	maxSerialized  = 1048576
	maxNeeds       = 63
	maxWrites      = 64
	maxPromptBytes = 65536
	maxWriteBytes  = 1024
	maxSchemaBytes = 16384
	maxSchemaDepth = 16
)

type Graph struct {
	Nodes []Node `json:"nodes"`
}

type Node struct {
	ID      string         `json:"id"`
	Needs   []string       `json:"needs"`
	Profile string         `json:"profile"`
	Prompt  string         `json:"prompt"`
	Writes  []string       `json:"writes"`
	Schema  jsontext.Value `json:"schema,omitzero"`
}

func Parse(raw jsontext.Value) (Graph, error) {
	if len(raw) > maxSerialized {
		return Graph{}, errors.New("graph: value exceeds 1048576 bytes")
	}
	graph, err := decode(raw)
	if err != nil {
		return Graph{}, err
	}
	for i := range graph.Nodes {
		sort.Strings(graph.Nodes[i].Needs)
		sort.Strings(graph.Nodes[i].Writes)
	}
	if err := Validate(graph); err != nil {
		return Graph{}, err
	}
	return graph, nil
}

func Validate(value Graph) error {
	if len(value.Nodes) < 1 || len(value.Nodes) > maxNodes {
		return errors.New("graph: nodes must contain 1 to 64 nodes")
	}
	ids := make(map[string]struct{}, len(value.Nodes))
	for i := range value.Nodes {
		if err := checkNode(&value.Nodes[i], ids); err != nil {
			return err
		}
	}
	if err := checkNeedsResolve(value.Nodes); err != nil {
		return err
	}
	if err := checkAcyclic(value.Nodes); err != nil {
		return err
	}
	return checkOverlaps(value.Nodes)
}

func checkNode(node *Node, ids map[string]struct{}) error {
	if err := checkNodeHead(node, ids); err != nil {
		return err
	}
	if err := checkNodeNeeds(node); err != nil {
		return err
	}
	if err := checkNodeWrites(node); err != nil {
		return err
	}
	return checkNodeSchema(node)
}

func checkNodeHead(node *Node, ids map[string]struct{}) error {
	if !validName(node.ID) {
		return fmt.Errorf("graph: invalid node id %q", node.ID)
	}
	if _, exists := ids[node.ID]; exists {
		return fmt.Errorf("graph: duplicate node id %q", node.ID)
	}
	ids[node.ID] = struct{}{}
	if node.Profile != "" && !validName(node.Profile) {
		return fmt.Errorf("graph: invalid profile %q", node.Profile)
	}
	if !utf8.ValidString(node.Prompt) {
		return fmt.Errorf("graph: node %q prompt must be valid utf-8 within 65536 bytes", node.ID)
	}
	if len(node.Prompt) > maxPromptBytes {
		return fmt.Errorf("graph: node %q prompt must be valid utf-8 within 65536 bytes", node.ID)
	}
	if strings.TrimSpace(node.Prompt) == "" {
		return fmt.Errorf("graph: node %q needs a prompt", node.ID)
	}
	return nil
}

func checkNodeNeeds(node *Node) error {
	if len(node.Needs) > maxNeeds {
		return fmt.Errorf("graph: node %q exceeds 63 needs", node.ID)
	}
	seen := make(map[string]struct{}, len(node.Needs))
	for _, need := range node.Needs {
		if need == node.ID {
			return fmt.Errorf("graph: node %q needs itself", node.ID)
		}
		if _, exists := seen[need]; exists {
			return fmt.Errorf("graph: node %q repeats need %q", node.ID, need)
		}
		seen[need] = struct{}{}
	}
	return nil
}

func checkNodeWrites(node *Node) error {
	if len(node.Writes) > maxWrites {
		return fmt.Errorf("graph: node %q exceeds 64 writes", node.ID)
	}
	seen := make(map[string]struct{}, len(node.Writes))
	for _, write := range node.Writes {
		if !validWritePath(write) {
			return fmt.Errorf("graph: node %q has invalid write path %q", node.ID, write)
		}
		if _, exists := seen[write]; exists {
			return fmt.Errorf("graph: node %q repeats write path %q", node.ID, write)
		}
		seen[write] = struct{}{}
	}
	return nil
}

func checkNeedsResolve(nodes []Node) error {
	known := make(map[string]struct{}, len(nodes))
	for _, node := range nodes {
		known[node.ID] = struct{}{}
	}
	for i := range nodes {
		for _, need := range nodes[i].Needs {
			if _, ok := known[need]; !ok {
				return fmt.Errorf("graph: node %q has unknown need %q", nodes[i].ID, need)
			}
		}
	}
	return nil
}

func checkAcyclic(nodes []Node) error {
	index := make(map[string]int, len(nodes))
	for i, node := range nodes {
		index[node.ID] = i
	}
	state := make([]int, len(nodes))
	var visit func(i int) bool
	visit = func(i int) bool {
		state[i] = 1
		for _, need := range nodes[i].Needs {
			j := index[need]
			if state[j] == 1 {
				return false
			}
			if state[j] == 0 && !visit(j) {
				return false
			}
		}
		state[i] = 2
		return true
	}
	for i := range nodes {
		if state[i] == 0 && !visit(i) {
			return errors.New("graph: dependency cycle")
		}
	}
	return nil
}

func checkOverlaps(nodes []Node) error {
	byID := make(map[string]Node, len(nodes))
	for _, node := range nodes {
		byID[node.ID] = node
	}
	for i := range nodes {
		for j := i + 1; j < len(nodes); j++ {
			if err := checkOverlapPair(nodes[i], nodes[j], byID); err != nil {
				return err
			}
		}
	}
	return nil
}

func checkOverlapPair(a, b Node, byID map[string]Node) error {
	if reachesNode(byID, a.ID, b.ID) || reachesNode(byID, b.ID, a.ID) {
		return nil
	}
	if path, ok := firstOverlap(a.Writes, b.Writes); ok {
		return fmt.Errorf("graph: unordered writes overlap between %q and %q at %q", a.ID, b.ID, path)
	}
	return nil
}

func reachesNode(byID map[string]Node, from, target string) bool {
	seen := map[string]struct{}{}
	stack := []string{from}
	for len(stack) > 0 {
		id := stack[len(stack)-1]
		stack = stack[:len(stack)-1]
		if id == target {
			return true
		}
		if _, ok := seen[id]; ok {
			continue
		}
		seen[id] = struct{}{}
		stack = append(stack, byID[id].Needs...)
	}
	return false
}

func firstOverlap(a, b []string) (string, bool) {
	best := ""
	for _, wa := range a {
		for _, wb := range b {
			if path, ok := overlapPath(wa, wb); ok && (best == "" || path < best) {
				best = path
			}
		}
	}
	if best == "" {
		return "", false
	}
	return best, true
}

func overlapPath(a, b string) (string, bool) {
	if a == "." || b == "." {
		return ".", true
	}
	pa := strings.Split(a, "/")
	pb := strings.Split(b, "/")
	n := min(len(pa), len(pb))
	for i := 0; i < n; i++ {
		if pa[i] != pb[i] {
			return "", false
		}
	}
	if len(pa) <= len(pb) {
		return a, true
	}
	return b, true
}

func validName(s string) bool {
	if len(s) == 0 || len(s) > 48 {
		return false
	}
	if s[0] < 'a' || s[0] > 'z' {
		return false
	}
	for i := 1; i < len(s); i++ {
		if !isNameRune(s[i]) {
			return false
		}
	}
	return true
}

func isNameRune(c byte) bool {
	return c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '_' || c == '-'
}

func validWritePath(w string) bool {
	if w == "." {
		return true
	}
	if w == "" || len(w) > maxWriteBytes || !utf8.ValidString(w) {
		return false
	}
	if hasBadWriteChars(w) {
		return false
	}
	for _, part := range strings.Split(w, "/") {
		if !validWritePart(part) {
			return false
		}
	}
	return !hasControlRune(w)
}

func hasControlRune(w string) bool {
	for _, r := range w {
		if unicode.IsControl(r) {
			return true
		}
	}
	return false
}

func hasBadWriteChars(w string) bool {
	if strings.HasPrefix(w, "/") || strings.Contains(w, "\\") || strings.ContainsRune(w, 0) {
		return true
	}
	return len(w) >= 2 && w[1] == ':'
}

func validWritePart(part string) bool {
	if part == "" || part == "." || part == ".." {
		return false
	}
	return !strings.EqualFold(part, ".git")
}

func decode(raw jsontext.Value) (Graph, error) {
	var graph Graph
	fields, err := decodeMembers(raw, graphMembers)
	if err != nil {
		return Graph{}, err
	}
	if err := decodeNodes(fields["nodes"], &graph.Nodes); err != nil {
		return Graph{}, err
	}
	return graph, nil
}

func graphMembers(name string) bool {
	return name == "nodes"
}

var nodeMembers = map[string]bool{
	"id": true, "needs": true, "profile": true,
	"prompt": true, "writes": true, "schema": true,
}

func decodeMembers(raw jsontext.Value, allowed func(string) bool) (map[string]jsontext.Value, error) {
	reject := func(fields map[string]jsontext.Value, member string) error {
		if _, exists := fields[member]; exists {
			return fmt.Errorf("graph: duplicate member %q", member)
		}
		if !allowed(member) {
			return fmt.Errorf("graph: unknown member %q", member)
		}
		return nil
	}
	return decodeMemberFields(raw, reject)
}

func decodeMemberFields(raw jsontext.Value, reject func(map[string]jsontext.Value, string) error) (map[string]jsontext.Value, error) {
	if len(raw) == 0 {
		return nil, errors.New("graph: invalid value")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginObject {
		return nil, errors.New("graph: invalid value")
	}
	fields, err := readMemberFields(decoder, reject)
	if err != nil {
		return nil, err
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, errors.New("graph: invalid value")
	}
	if _, err := decoder.ReadToken(); !errors.Is(err, io.EOF) {
		return nil, errors.New("graph: invalid value")
	}
	return fields, nil
}

func readMemberFields(decoder *jsontext.Decoder, reject func(map[string]jsontext.Value, string) error) (map[string]jsontext.Value, error) {
	fields := make(map[string]jsontext.Value)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, errors.New("graph: invalid value")
		}
		member := name.String()
		if memberErr := reject(fields, member); memberErr != nil {
			return nil, memberErr
		}
		value, err := decoder.ReadValue()
		if err != nil {
			return nil, errors.New("graph: invalid value")
		}
		fields[member] = bytes.Clone(value)
	}
	return fields, nil
}

func decodeNodes(raw jsontext.Value, nodes *[]Node) error {
	if len(raw) == 0 {
		return errors.New("graph: invalid value")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginArray {
		return errors.New("graph: invalid value")
	}
	for decoder.PeekKind() != jsontext.KindEndArray {
		var node Node
		if err := decodeNode(decoder, &node); err != nil {
			return err
		}
		*nodes = append(*nodes, node)
	}
	if _, err := decoder.ReadToken(); err != nil {
		return errors.New("graph: invalid value")
	}
	return nil
}

func decodeNode(decoder *jsontext.Decoder, node *Node) error {
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginObject {
		return errors.New("graph: invalid value")
	}
	seen := map[string]bool{}
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return errors.New("graph: invalid value")
		}
		member := name.String()
		if seen[member] {
			return fmt.Errorf("graph: duplicate member %q", member)
		}
		seen[member] = true
		if !nodeMembers[member] {
			return fmt.Errorf("graph: unknown member %q", member)
		}
		if err := decodeNodeMember(decoder, node, member); err != nil {
			return err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return errors.New("graph: invalid value")
	}
	return nil
}

func decodeNodeMember(decoder *jsontext.Decoder, node *Node, member string) error {
	switch member {
	case "id", "profile", "prompt":
		return decodeStringNodeMember(decoder, node, member)
	case "needs", "writes":
		var list []string
		if err := decodeStringArray(decoder, &list); err != nil {
			return err
		}
		if member == "needs" {
			node.Needs = list
		} else {
			node.Writes = list
		}
	case "schema":
		value, err := decoder.ReadValue()
		if err != nil {
			return errors.New("graph: invalid value")
		}
		node.Schema = bytes.Clone(value)
	}
	return nil
}

func decodeStringNodeMember(decoder *jsontext.Decoder, node *Node, member string) error {
	value, err := decoder.ReadValue()
	if err != nil {
		return errors.New("graph: invalid value")
	}
	str, err := decodeJSONString(value)
	if err != nil {
		return errors.New("graph: invalid value")
	}
	switch member {
	case "id":
		node.ID = str
	case "profile":
		node.Profile = str
	case "prompt":
		node.Prompt = str
	}
	return nil
}

func decodeStringArray(decoder *jsontext.Decoder, out *[]string) error {
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginArray {
		return errors.New("graph: invalid value")
	}
	for decoder.PeekKind() != jsontext.KindEndArray {
		value, err := decoder.ReadValue()
		if err != nil {
			return errors.New("graph: invalid value")
		}
		str, err := decodeJSONString(value)
		if err != nil {
			return errors.New("graph: invalid value")
		}
		*out = append(*out, str)
	}
	if _, err := decoder.ReadToken(); err != nil {
		return errors.New("graph: invalid value")
	}
	return nil
}

func decodeJSONString(raw jsontext.Value) (string, error) {
	if len(raw) == 0 {
		return "", errors.New("not a string")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindString {
		return "", errors.New("not a string")
	}
	return token.String(), nil
}

func checkNodeSchema(node *Node) error {
	if len(node.Schema) == 0 {
		return nil
	}
	if len(node.Schema) > maxSchemaBytes {
		return errors.New("task: output schema exceeds limit")
	}
	return checkSchemaNode(node.Schema, 0)
}

var schemaKeywords = map[string]bool{
	"type": true, "properties": true, "required": true,
	"items": true, "enum": true, "additionalProperties": true,
}

func checkSchemaNode(raw jsontext.Value, depth int) error {
	if depth > maxSchemaDepth {
		return errors.New("task: output schema exceeds limit")
	}
	fields, err := decodeSchemaMembers(raw)
	if err != nil {
		return err
	}
	if err := checkSchemaKeywords(fields); err != nil {
		return err
	}
	return checkSchemaLinks(fields, depth)
}

func checkSchemaKeywords(fields map[string]jsontext.Value) error {
	if value, ok := fields["type"]; ok {
		if err := checkSchemaType(value); err != nil {
			return err
		}
	}
	if value, ok := fields["enum"]; ok {
		if err := checkSchemaEnum(value); err != nil {
			return err
		}
	}
	if value, ok := fields["additionalProperties"]; ok {
		if err := checkSchemaAdditional(value); err != nil {
			return err
		}
	}
	return nil
}

func checkSchemaLinks(fields map[string]jsontext.Value, depth int) error {
	var props map[string]bool
	if value, ok := fields["properties"]; ok {
		var err error
		props, err = checkSchemaProperties(value, depth)
		if err != nil {
			return err
		}
	}
	if value, ok := fields["required"]; ok {
		if err := checkSchemaRequired(value, props); err != nil {
			return err
		}
	}
	if value, ok := fields["items"]; ok {
		return checkSchemaNode(value, depth+1)
	}
	return nil
}

func decodeSchemaMembers(raw jsontext.Value) (map[string]jsontext.Value, error) {
	reject := func(fields map[string]jsontext.Value, member string) error {
		if _, exists := fields[member]; exists {
			return fmt.Errorf("graph: duplicate member %q", member)
		}
		if !schemaKeywords[member] {
			return fmt.Errorf("task: unsupported schema keyword %q", member)
		}
		return nil
	}
	return decodeMemberFields(raw, reject)
}

func checkSchemaType(raw jsontext.Value) error {
	s, err := decodeJSONString(raw)
	if err != nil {
		return errors.New("graph: invalid value")
	}
	switch s {
	case "object", "array", "string", "number", "integer", "boolean", "null":
		return nil
	}
	return errors.New("graph: invalid value")
}

func checkSchemaProperties(raw jsontext.Value, depth int) (map[string]bool, error) {
	if len(raw) == 0 {
		return nil, errors.New("graph: invalid value")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginObject {
		return nil, errors.New("graph: invalid value")
	}
	props := make(map[string]bool)
	for decoder.PeekKind() != jsontext.KindEndObject {
		name, err := decoder.ReadToken()
		if err != nil {
			return nil, errors.New("graph: invalid value")
		}
		if err := checkSchemaProperty(decoder, props, name.String(), depth); err != nil {
			return nil, err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return nil, errors.New("graph: invalid value")
	}
	return props, nil
}

func checkSchemaProperty(decoder *jsontext.Decoder, props map[string]bool, prop string, depth int) error {
	if props[prop] {
		return fmt.Errorf("graph: duplicate member %q", prop)
	}
	props[prop] = true
	value, err := decoder.ReadValue()
	if err != nil {
		return errors.New("graph: invalid value")
	}
	return checkSchemaNode(value, depth+1)
}

func checkSchemaRequired(raw jsontext.Value, props map[string]bool) error {
	var names []string
	if err := decodeStringArrayBytes(raw, &names); err != nil {
		return errors.New("graph: invalid value")
	}
	seen := make(map[string]bool, len(names))
	for _, name := range names {
		if seen[name] || !props[name] {
			return errors.New("graph: invalid value")
		}
		seen[name] = true
	}
	return nil
}

func decodeStringArrayBytes(raw jsontext.Value, out *[]string) error {
	if len(raw) == 0 {
		return errors.New("not an array")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginArray {
		return errors.New("not an array")
	}
	for decoder.PeekKind() != jsontext.KindEndArray {
		value, err := decoder.ReadValue()
		if err != nil {
			return err
		}
		str, err := decodeJSONString(value)
		if err != nil {
			return err
		}
		*out = append(*out, str)
	}
	if _, err := decoder.ReadToken(); err != nil {
		return err
	}
	return nil
}

func checkSchemaEnum(raw jsontext.Value) error {
	if len(raw) == 0 {
		return errors.New("graph: invalid value")
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindBeginArray {
		return errors.New("graph: invalid value")
	}
	count := 0
	for decoder.PeekKind() != jsontext.KindEndArray {
		if _, err := decoder.ReadValue(); err != nil {
			return errors.New("graph: invalid value")
		}
		count++
	}
	if _, err := decoder.ReadToken(); err != nil {
		return errors.New("graph: invalid value")
	}
	if count == 0 {
		return errors.New("graph: invalid value")
	}
	return nil
}

func checkSchemaAdditional(raw jsontext.Value) error {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw), jsontext.AllowDuplicateNames(true))
	token, err := decoder.ReadToken()
	if err != nil || token.Kind() != jsontext.KindFalse {
		return errors.New("task: additionalProperties must be false")
	}
	return nil
}
