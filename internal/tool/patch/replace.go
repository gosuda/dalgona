package patch

import (
	"encoding/json/jsontext"
	"errors"
	"fmt"
)

type replacementChange struct {
	path    string
	old     *string
	line    *int
	all     bool
	newBody *string
	tag     *string
	create  *string
	delete  bool
	rename  *string
}

func parseReplace(input string) ([]edit, error) {
	items, err := replaceItems(input)
	if err != nil {
		return nil, err
	}
	out := make([]edit, 0, len(items))
	for i, raw := range items {
		change, decodeErr := decodeReplacement(raw)
		if decodeErr != nil {
			return nil, fmt.Errorf("patch: changes[%d]: %w", i, decodeErr)
		}
		lowered, lowerErr := lowerReplacement(i, change)
		if lowerErr != nil {
			return nil, lowerErr
		}
		out = append(out, lowered)
	}
	return out, nil
}

func replaceItems(input string) ([]jsontext.Value, error) {
	object, err := decodeObject(jsontext.Value(input))
	if err != nil {
		return nil, fmt.Errorf("patch: invalid input: %w", err)
	}
	if len(object) != 1 || object["changes"] == nil {
		return nil, errors.New("patch: invalid input: expected only changes")
	}
	items, err := decodeArray(object["changes"])
	if err != nil {
		return nil, fmt.Errorf("patch: invalid input: changes must be an array: %w", err)
	}
	if len(items) < 1 || len(items) > 64 {
		return nil, errors.New("patch: changes must contain 1 to 64 entries")
	}
	return items, nil
}

func knownReplacementField(name string) bool {
	switch name {
	case "path", "old", "line", "all", "new", "tag", "create", "delete", "rename":
		return true
	default:
		return false
	}
}

func decodeReplacement(raw jsontext.Value) (replacementChange, error) {
	object, err := decodeObject(raw)
	if err != nil {
		return replacementChange{}, err
	}
	for name := range object {
		if !knownReplacementField(name) {
			return replacementChange{}, fmt.Errorf("unknown field %q", name)
		}
	}
	var out replacementChange
	out.path, err = jsonString(object["path"])
	if err != nil {
		return replacementChange{}, fmt.Errorf("path must be a string: %w", err)
	}
	if err := decodeReplacementFields(object, &out); err != nil {
		return replacementChange{}, err
	}
	return out, nil
}

func decodeReplacementFields(object map[string]jsontext.Value, out *replacementChange) error {
	if value, ok, err := optionalString(object, "old"); err != nil {
		return fmt.Errorf("old must be a string: %w", err)
	} else if ok {
		out.old = &value
	}
	if value, ok, err := optionalInt(object, "line"); err != nil || value < 1 && ok {
		return errors.New("line must be a positive integer")
	} else if ok {
		out.line = &value
	}
	if value, err := optionalBool(object, "all"); err != nil {
		return fmt.Errorf("all must be a boolean: %w", err)
	} else {
		out.all = value
	}
	return decodeReplacementActions(object, out)
}

func decodeReplacementActions(object map[string]jsontext.Value, out *replacementChange) error {
	for _, item := range []struct {
		name string
		dest **string
	}{{"new", &out.newBody}, {"tag", &out.tag}, {"create", &out.create}, {"rename", &out.rename}} {
		value, ok, err := optionalString(object, item.name)
		if err != nil {
			return fmt.Errorf("%s must be a string: %w", item.name, err)
		}
		if ok {
			*item.dest = &value
		}
	}
	value, err := optionalBool(object, "delete")
	if err != nil {
		return fmt.Errorf("delete must be a boolean: %w", err)
	}
	out.delete = value
	return nil
}

func lowerReplacement(index int, change replacementChange) (edit, error) {
	if actions := change.actionCount(); actions != 1 {
		return edit{}, replacementActionError(index, actions, change)
	}
	base := edit{index: index, path: change.path, window: beforePayload}
	switch {
	case change.create != nil:
		base.kind, base.body = editCreate, *change.create
	case change.delete:
		base.kind = editDelete
	case change.rename != nil:
		base.kind, base.to = editRename, *change.rename
	default:
		return lowerReplacementText(base, change)
	}
	if change.textFieldsSet() {
		return edit{}, fmt.Errorf("patch: changes[%d]: lifecycle action cannot be used with old, new, tag, line, or all", index)
	}
	return base, nil
}

func (c replacementChange) textFieldsSet() bool {
	return c.old != nil || c.newBody != nil || c.tag != nil || c.line != nil || c.all
}

func (c replacementChange) actionCount() int {
	count := 0
	for _, present := range []bool{c.old != nil || c.tag != nil || c.newBody != nil, c.create != nil, c.delete, c.rename != nil} {
		if present {
			count++
		}
	}
	return count
}

func replacementActionError(index, count int, change replacementChange) error {
	if count == 0 {
		return fmt.Errorf("patch: changes[%d]: say what to change: old and new, tag and new, create, delete, or rename", index)
	}
	first, second := "old and new", "create"
	if change.create == nil {
		first, second = "create", "delete"
	}
	if change.rename != nil {
		second = "rename"
	}
	return fmt.Errorf("patch: changes[%d]: %s cannot be used with %s", index, first, second)
}

func lowerReplacementText(base edit, change replacementChange) (edit, error) {
	index := base.index
	if change.old != nil && change.tag != nil && !change.all {
		return edit{}, fmt.Errorf("patch: changes[%d]: old cannot be used with tag", index)
	}
	if change.newBody == nil {
		return edit{}, fmt.Errorf("patch: changes[%d]: old or tag needs new", index)
	}
	base.kind, base.body, base.action = editChange, *change.newBody, actionReplace
	base.guard = wholeProof(change.tag)
	if change.old == nil {
		if change.line != nil || change.all {
			return edit{}, fmt.Errorf("patch: changes[%d]: whole-file replacement cannot be used with line or all", index)
		}
		base.locator.kind = locatorWhole
		return base, nil
	}
	return lowerOldReplace(base, change)
}

func lowerOldReplace(base edit, change replacementChange) (edit, error) {
	if *change.old == "" {
		return edit{}, fmt.Errorf("patch: changes[%d]: old must not be empty", base.index)
	}
	base.locator = locator{kind: locatorText, old: *change.old, all: change.all}
	if change.line != nil {
		base.locator.lineHint = *change.line
	}
	if !change.all {
		base.guard = guard{kind: guardQuoted}
	}
	return base, nil
}

func wholeProof(tag *string) guard {
	if tag == nil {
		return guard{kind: guardSeen}
	}
	return guard{kind: guardWholeTag, text: *tag}
}
