package ratchet

import "math"

type summary struct {
	N    int
	Mean *float64
	Half *float64
}

type delta struct {
	summary
	Verdict string
}

type caseMeans map[string]float64

var tCritical95 = [...]float64{
	12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228,
	2.201, 2.179, 2.160, 2.145, 2.131, 2.120, 2.110, 2.101, 2.093, 2.086,
	2.080, 2.074, 2.069, 2.064, 2.060, 2.056, 2.052, 2.048, 2.045, 2.042,
}

func tCritical(df int) float64 {
	if df > len(tCritical95) {
		return 1.96
	}
	return tCritical95[df-1]
}

func meanAndHalf(values []float64) summary {
	s := summary{N: len(values)}
	if len(values) == 0 {
		return s
	}
	sum := 0.0
	for _, v := range values {
		sum += v
	}
	mean := sum / float64(len(values))
	s.Mean = &mean
	if len(values) < 2 {
		return s
	}
	squares := 0.0
	for _, v := range values {
		squares += (v - mean) * (v - mean)
	}
	se := math.Sqrt(squares / float64(len(values)-1) / float64(len(values)))
	half := tCritical(len(values)-1) * se
	s.Half = &half
	return s
}

func verdict(s summary, direction string) string {
	if s.N < 2 || s.Mean == nil || s.Half == nil {
		return "flat"
	}
	gain := *s.Mean
	if direction == "lower" {
		gain = -gain
	}
	switch {
	case gain-*s.Half > 0:
		return "up"
	case gain+*s.Half < 0:
		return "down"
	default:
		return "flat"
	}
}

func paired(cand, ref caseMeans, ids []string, direction string) delta {
	var diffs []float64
	for _, id := range ids {
		c, inCand := cand[id]
		r, inRef := ref[id]
		if inCand && inRef {
			diffs = append(diffs, c-r)
		}
	}
	s := meanAndHalf(diffs)
	return delta{summary: s, Verdict: verdict(s, direction)}
}
