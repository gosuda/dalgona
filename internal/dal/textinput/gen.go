//go:build ignore

package main

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"os"
	"slices"
	"strconv"
	"strings"
)

const base = "https://www.unicode.org/Public/18.0.0/ucd/"

var pinned = map[string]string{
	"auxiliary/GraphemeBreakProperty.txt": "0839dcb79e4ac639ecd538b1abf7c9d22e3f9dd265b7e182d33627aa4d75b45a",
	"emoji/emoji-data.txt":                "80d00f8e616a0ef27fd6b8de3b758c06383b5d917e2977709578e68baf733bf1",
	"DerivedCoreProperties.txt":           "09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9",
	"PropList.txt":                        "f438f532e8737bb8a2702126cdf9c4af5e357c58c7acf9d9eb2fc7c1a1d955d6",
}

var gcbValues = []string{"Other", "CR", "LF", "Control", "Extend", "ZWJ", "Regional_Indicator", "Prepend", "SpacingMark", "L", "V", "T", "LV", "LVT"}

var incbValues = []string{"None", "Linker", "Consonant", "Extend"}

type span struct {
	lo, hi rune
	value  string
}

func main() {
	files := map[string][]byte{}
	for name, sum := range pinned {
		data, err := fetch(base + name)
		if err != nil {
			fail(err)
		}
		got := sha256.Sum256(data)
		if hex.EncodeToString(got[:]) != sum {
			fail(fmt.Errorf("%s: sha256 %x, want %s", name, got, sum))
		}
		files[name] = data
	}
	props := make([]uint8, 0x110000)
	apply(props, parse(files["auxiliary/GraphemeBreakProperty.txt"], ""), func(old uint8, v string) uint8 {
		return old&^0x0f | uint8(slices.Index(gcbValues, v))
	})
	apply(props, parse(files["emoji/emoji-data.txt"], "Extended_Pictographic"), func(old uint8, _ string) uint8 { return old | 0x10 })
	apply(props, parse(files["DerivedCoreProperties.txt"], "InCB"), func(old uint8, v string) uint8 {
		return old&^0x60 | uint8(slices.Index(incbValues, v))<<5
	})
	apply(props, parse(files["DerivedCoreProperties.txt"], "Default_Ignorable_Code_Point"), func(old uint8, _ string) uint8 { return old | 0x80 })
	white := make([]uint8, 0x110000)
	apply(white, parse(files["PropList.txt"], "White_Space"), func(uint8, string) uint8 { return 1 })
	propRanges, whiteRanges := ranges(props), ranges(white)
	if err := os.WriteFile("tables.go", goTables(propRanges, whiteRanges), 0o644); err != nil {
		fail(err)
	}
	if err := os.WriteFile("../../../web/src/lib/text/tables.ts", tsTables(propRanges, whiteRanges), 0o644); err != nil {
		fail(err)
	}
}

func fetch(url string) ([]byte, error) {
	resp, err := http.Get(url) //nolint:gosec,noctx // fixed pinned host, generator only
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("%s: %s", url, resp.Status)
	}
	return io.ReadAll(resp.Body)
}

func parse(data []byte, property string) []span {
	var out []span
	scanner := bufio.NewScanner(bytes.NewReader(data))
	for scanner.Scan() {
		line, _, _ := strings.Cut(scanner.Text(), "#")
		fields := strings.Split(line, ";")
		if len(fields) < 2 {
			continue
		}
		for i := range fields {
			fields[i] = strings.TrimSpace(fields[i])
		}
		value := fields[1]
		if property != "" {
			if fields[1] != property {
				continue
			}
			value = "Yes"
			if len(fields) > 2 {
				value = fields[2]
			}
		}
		lo, hi, ok := strings.Cut(fields[0], "..")
		if !ok {
			hi = lo
		}
		out = append(out, span{lo: hexRune(lo), hi: hexRune(hi), value: value})
	}
	return out
}

func hexRune(s string) rune {
	v, err := strconv.ParseUint(s, 16, 32)
	if err != nil {
		fail(err)
	}
	return rune(v)
}

func apply(table []uint8, spans []span, set func(uint8, string) uint8) {
	for _, s := range spans {
		for r := s.lo; r <= s.hi; r++ {
			table[r] = set(table[r], s.value)
		}
	}
}

type rng struct {
	lo, hi rune
	value  uint8
}

func ranges(table []uint8) []rng {
	var out []rng
	for r := rune(0); r < rune(len(table)); r++ {
		v := table[r]
		if v == 0 {
			continue
		}
		if n := len(out); n > 0 && out[n-1].hi == r-1 && out[n-1].value == v {
			out[n-1].hi = r
			continue
		}
		out = append(out, rng{lo: r, hi: r, value: v})
	}
	return out
}

func goTables(props, white []rng) []byte {
	var b bytes.Buffer
	fmt.Fprintf(&b, "package textinput\n\n")
	fmt.Fprintf(&b, "var propRanges = [...]propRange{\n")
	for _, r := range props {
		fmt.Fprintf(&b, "\t{0x%04X, 0x%04X, 0x%02X},\n", r.lo, r.hi, r.value)
	}
	fmt.Fprintf(&b, "}\n\nvar whiteSpaceRanges = [...]propRange{\n")
	for _, r := range white {
		fmt.Fprintf(&b, "\t{0x%04X, 0x%04X, 1},\n", r.lo, r.hi)
	}
	fmt.Fprintf(&b, "}\n")
	return b.Bytes()
}

func tsTables(props, white []rng) []byte {
	var b bytes.Buffer
	fmt.Fprintf(&b, "export const propRanges: readonly number[] = [\n")
	for _, r := range props {
		fmt.Fprintf(&b, "\t0x%04x, 0x%04x, 0x%02x,\n", r.lo, r.hi, r.value)
	}
	fmt.Fprintf(&b, "];\n\nexport const whiteSpaceRanges: readonly number[] = [\n")
	for _, r := range white {
		fmt.Fprintf(&b, "\t0x%04x, 0x%04x,\n", r.lo, r.hi)
	}
	fmt.Fprintf(&b, "];\n")
	return b.Bytes()
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "gen:", err)
	os.Exit(1)
}
