package ratchet

import (
	"encoding/json/jsontext"
	"fmt"
	"path/filepath"
	"strings"
)

type trainView struct {
	History map[string]map[string]*float64 `json:"history"`
	Rows    []jsontext.Value               `json:"rows"`
	Traces  []string                       `json:"traces"`
	Variant string                         `json:"variant"`
}

func (f *flow) train(s *state, variant string) (trainView, error) {
	if !s.gated(variant) {
		return trainView{}, fmt.Errorf("train needs a gated round for %s", variant)
	}
	data, err := f.readVariant(variant)
	if err != nil {
		return trainView{}, err
	}
	train := idSet(s.TrainIDs)
	view := trainView{
		Variant: variant,
		Rows:    trainRows(data.Rows, train),
		Traces:  f.trainTraces(variant, data.Traces, train),
	}
	view.History, err = f.trainHistory(s, train)
	if err != nil {
		return trainView{}, err
	}
	return view, nil
}

func trainRows(rows []resultRow, train map[string]bool) []jsontext.Value {
	out := []jsontext.Value{}
	for _, row := range rows {
		if train[row.CaseID] {
			out = append(out, row.Raw)
		}
	}
	return out
}

func (f *flow) trainTraces(variant string, names []string, train map[string]bool) []string {
	out := []string{}
	for _, name := range names {
		if m := traceName.FindStringSubmatch(name); m != nil && train[m[1]] {
			out = append(out, filepath.ToSlash(filepath.Join(f.dir, variant, "traces", name)))
		}
	}
	return out
}

func (f *flow) trainHistory(s *state, train map[string]bool) (map[string]map[string]*float64, error) {
	history := make(map[string]map[string]*float64, len(s.TrainIDs))
	for _, id := range s.TrainIDs {
		history[id] = make(map[string]*float64, len(s.Rounds))
	}
	for _, v := range s.variants() {
		data, err := f.readVariant(v)
		if err != nil {
			return nil, err
		}
		means := meansByCase(filterRows(data.Rows, train), s.Goal.Target)
		for _, id := range s.TrainIDs {
			var value *float64
			if mean, ok := means[id]; ok {
				value = new(mean)
			}
			history[id][v] = value
		}
	}
	return history, nil
}

func renderTable(s *state) string {
	target := s.Goal.Target
	cols := []string{"round", "variant", "change", "decision", "train " + target, "test " + target}
	for _, m := range s.Goal.Hold {
		cols = append(cols, "train "+m, "test "+m)
	}
	var b strings.Builder
	fmt.Fprintf(&b, "| %s |\n", strings.Join(cols, " | "))
	fmt.Fprintf(&b, "|%s\n", strings.Repeat("---|", len(cols)))
	for _, r := range s.Rounds {
		cells := []string{fmt.Sprint(r.Round), variantCell(s, r.Variant), strings.ReplaceAll(r.Change, "|", `\|`), r.Decision}
		cells = append(cells, summaryCell(r.Train[target]), summaryCell(r.Test[target]))
		for _, m := range s.Goal.Hold {
			cells = append(cells, summaryCell(r.Train[m]), summaryCell(r.Test[m]))
		}
		fmt.Fprintf(&b, "| %s |\n", strings.Join(cells, " | "))
	}
	return b.String()
}

func variantCell(s *state, variant string) string {
	if s.Best != nil && s.Best.Variant == variant {
		return variant + " (best)"
	}
	return variant
}

func summaryCell(s summary) string {
	switch {
	case s.Mean == nil:
		return "–"
	case s.Half == nil:
		return fmt.Sprintf("%.3f", *s.Mean)
	}
	return fmt.Sprintf("%.3f ±%.3f", *s.Mean, *s.Half)
}
