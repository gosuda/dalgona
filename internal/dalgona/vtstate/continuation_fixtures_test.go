package vtstate

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

type continuationCase struct {
	name             string
	suffix           string
	prefixPrimary0   string
	prefixPrimary1   string
	prefixAlt0       string
	prefixAlt1       string
	prefixX          int
	prefixY          int
	prefixPending    string
	prefixHasPending bool
	suffixPrimary0   string
	suffixPrimary1   string
	suffixAlt0       string
	suffixAlt1       string
	suffixX          int
	suffixY          int
	suffixPending    string
	suffixHasPending bool
	prefixG0         string
	prefixSingle     int
	prefixVisible    bool
	prefixShape      CursorShape
	prefixActive     string
	prefixSavedX     int
	prefixSavedY     int
	suffixG0         string
	suffixSingle     int
	suffixVisible    bool
	suffixShape      CursorShape
	suffixActive     string
	suffixSavedX     int
	suffixSavedY     int
	suffixLink       string
}

type termWant struct {
	g0      string
	single  int
	visible bool
	shape   CursorShape
	active  string
}

var continuationCases = []continuationCase{
	{name: "ground_empty", suffix: "Hi!", prefixPrimary0: "      ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 0, prefixY: 0, prefixPending: "", prefixHasPending: false, suffixPrimary0: "Hi    ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 2, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "ground_pending_ascii", suffix: "C!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABC   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "cluster_combining", suffix: "!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "é", prefixHasPending: true, suffixPrimary0: "ABé   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "cluster_regional", suffix: "!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "🇰", prefixHasPending: true, suffixPrimary0: "AB🇰  ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 4, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "cluster_zwj", suffix: "!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "👩‍👩‍👧", prefixHasPending: true, suffixPrimary0: "AB👩‍👩‍👧  ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 4, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "utf8_2byte", suffix: "\xa9!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABé   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "utf8_3byte_lead", suffix: "\x82\xac!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "AB€   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "utf8_3byte_partial", suffix: "\xac!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "AB€   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "utf8_4byte_partial", suffix: "\x80!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "AB😀  ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 4, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "escape", suffix: "(0Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "special", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "escape_intermediate", suffix: "0Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "special", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "csi_entry", suffix: "2;1HZ!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "AB    ", suffixPrimary1: "Z     ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 1, suffixY: 1, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "csi_param", suffix: "1HZ!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "AB    ", suffixPrimary1: "Z     ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 1, suffixY: 1, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "csi_param_private", suffix: "lZ!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: false, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "csi_intermediate", suffix: "qZ!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorUnderline, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "dcs_entry", suffix: "qhello\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "dcs_intermediate", suffix: "qhello\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "dcs_param", suffix: "qhello\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "dcs_string", suffix: "\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "osc_title", suffix: "\x07Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "osc_hyperlink", suffix: "\x07Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: "https://example.com"},
	{name: "sos_string", suffix: "\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "pm_string", suffix: "\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "apc_string", suffix: "\x1b\\Z!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "c1_csi_entry", suffix: "2;1HZ!", prefixPrimary0: "A     ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 1, prefixY: 0, prefixPending: "B", prefixHasPending: true, suffixPrimary0: "AB    ", suffixPrimary1: "Z     ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 1, suffixY: 1, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "charset_g0_special", suffix: "Z!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "", prefixHasPending: false, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "special", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "special", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "charset_single_shift", suffix: "q!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "", prefixHasPending: false, suffixPrimary0: "ABq   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 2, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "saved_cursor", suffix: "Z!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "", prefixHasPending: false, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 2, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 2, suffixSavedY: 0, suffixLink: ""},
	{name: "modes_invisible", suffix: "Z!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 2, prefixY: 0, prefixPending: "", prefixHasPending: false, suffixPrimary0: "ABZ   ", suffixPrimary1: "      ", suffixAlt0: "      ", suffixAlt1: "      ", suffixX: 3, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: false, prefixShape: CursorBlock, prefixActive: "primary", prefixSavedX: 0, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: false, suffixShape: CursorBlock, suffixActive: "primary", suffixSavedX: 0, suffixSavedY: 0, suffixLink: ""},
	{name: "alternate_screen", suffix: "Z!", prefixPrimary0: "AB    ", prefixPrimary1: "      ", prefixAlt0: "      ", prefixAlt1: "      ", prefixX: 0, prefixY: 0, prefixPending: "", prefixHasPending: false, suffixPrimary0: "AB    ", suffixPrimary1: "      ", suffixAlt0: "Z     ", suffixAlt1: "      ", suffixX: 1, suffixY: 0, suffixPending: "!", suffixHasPending: true, prefixG0: "ascii", prefixSingle: 0, prefixVisible: true, prefixShape: CursorBlock, prefixActive: "alternate", prefixSavedX: 2, prefixSavedY: 0, suffixG0: "ascii", suffixSingle: 0, suffixVisible: true, suffixShape: CursorBlock, suffixActive: "alternate", suffixSavedX: 2, suffixSavedY: 0, suffixLink: ""},
}

func TestContinuationFixtures(t *testing.T) {
	for _, c := range continuationCases {
		t.Run(c.name, func(t *testing.T) {
			body := readContinuationFixture(t, c.name)
			term := restoreContinuation(t, body)
			checkCanonical(t, term, body)
			checkPrefixState(t, term, c)
			feedContinuationSuffix(t, term, c)
			checkSuffixState(t, term, c)
		})
	}
}

func readContinuationFixture(t *testing.T, name string) []byte {
	t.Helper()
	body, err := os.ReadFile(filepath.Join("testdata", "fixtures", name+".json"))
	if err != nil {
		t.Fatalf("read %s: %v", name, err)
	}
	return body
}

func restoreContinuation(t *testing.T, body []byte) *Terminal {
	t.Helper()
	term, err := Restore(body)
	if err != nil {
		t.Fatalf("Restore: %v", err)
	}
	return term
}

func checkCanonical(t *testing.T, term *Terminal, body []byte) {
	t.Helper()
	got, err := term.Checkpoint()
	if err != nil {
		t.Fatalf("Checkpoint: %v", err)
	}
	if !bytes.Equal(got, body) {
		t.Fatalf("re-encoded differs: got %d want %d", len(got), len(body))
	}
}

func feedContinuationSuffix(t *testing.T, term *Terminal, c continuationCase) {
	t.Helper()
	if _, err := term.Write([]byte(c.suffix)); err != nil {
		t.Fatalf("suffix write: %v", err)
	}
}

func checkPrefixState(t *testing.T, term *Terminal, c continuationCase) {
	t.Helper()
	checkPrimaryRows(t, term, c.prefixPrimary0, c.prefixPrimary1)
	checkAlternateRows(t, term, c.prefixAlt0, c.prefixAlt1)
	checkCursorPos(t, term, c.prefixX, c.prefixY)
	checkPendingText(t, term, c.prefixPending, c.prefixHasPending)
	checkTerminalState(t, term, prefixWant(c))
	checkSavedPos(t, term, c.prefixSavedX, c.prefixSavedY)
}

func checkSuffixState(t *testing.T, term *Terminal, c continuationCase) {
	t.Helper()
	checkPrimaryRows(t, term, c.suffixPrimary0, c.suffixPrimary1)
	checkAlternateRows(t, term, c.suffixAlt0, c.suffixAlt1)
	checkCursorPos(t, term, c.suffixX, c.suffixY)
	checkPendingText(t, term, c.suffixPending, c.suffixHasPending)
	checkTerminalState(t, term, suffixWant(c))
	checkSavedPos(t, term, c.suffixSavedX, c.suffixSavedY)
	checkSuffixLink(t, term, c)
}

func prefixWant(c continuationCase) termWant {
	return termWant{g0: c.prefixG0, single: c.prefixSingle, visible: c.prefixVisible, shape: c.prefixShape, active: c.prefixActive}
}

func suffixWant(c continuationCase) termWant {
	return termWant{g0: c.suffixG0, single: c.suffixSingle, visible: c.suffixVisible, shape: c.suffixShape, active: c.suffixActive}
}

func checkPrimaryRows(t *testing.T, term *Terminal, want0 string, want1 string) {
	t.Helper()
	if got := primaryRowText(term, 0); got != want0 {
		t.Fatalf("primary row 0 = %q want %q", got, want0)
	}
	if got := primaryRowText(term, 1); got != want1 {
		t.Fatalf("primary row 1 = %q want %q", got, want1)
	}
}

func checkAlternateRows(t *testing.T, term *Terminal, want0 string, want1 string) {
	t.Helper()
	if got := alternateRowText(term, 0); got != want0 {
		t.Fatalf("alternate row 0 = %q want %q", got, want0)
	}
	if got := alternateRowText(term, 1); got != want1 {
		t.Fatalf("alternate row 1 = %q want %q", got, want1)
	}
}

func primaryRowText(term *Terminal, y int) string {
	cells := term.screens[0].rows[y].cells
	var b strings.Builder
	for _, c := range cells {
		b.WriteString(c.Text)
	}
	return b.String()
}

func alternateRowText(term *Terminal, y int) string {
	cells := term.screens[1].rows[y].cells
	var b strings.Builder
	for _, c := range cells {
		b.WriteString(c.Text)
	}
	return b.String()
}

func checkCursorPos(t *testing.T, term *Terminal, x int, y int) {
	t.Helper()
	got := term.Cursor()
	if got.Column != x {
		t.Fatalf("column = %d want %d", got.Column, x)
	}
	if got.Row != y {
		t.Fatalf("row = %d want %d", got.Row, y)
	}
}

func checkPendingText(t *testing.T, term *Terminal, want string, has bool) {
	t.Helper()
	got, ok := term.PendingCell()
	if ok != has {
		t.Fatalf("pending ok = %v want %v", ok, has)
	}
	if !has {
		return
	}
	if got.Cell.Text != want {
		t.Fatalf("pending = %q want %q", got.Cell.Text, want)
	}
	if got.Column != term.Cursor().Column {
		t.Fatalf("pending column = %d want %d", got.Column, term.Cursor().Column)
	}
	if got.Row != term.Cursor().Row {
		t.Fatalf("pending row = %d want %d", got.Row, term.Cursor().Row)
	}
}

func checkTerminalState(t *testing.T, term *Terminal, want termWant) {
	t.Helper()
	checkCharsetG0(t, term, want.g0)
	checkSingleShift(t, term, want.single)
	checkVisibleShape(t, term, want.visible, want.shape)
	checkActiveScreen(t, term, want.active)
}

func checkCharsetG0(t *testing.T, term *Terminal, want string) {
	t.Helper()
	got := charsetNameString(term.charsets.designations[0])
	if got != want {
		t.Fatalf("g0 = %q want %q", got, want)
	}
}

func charsetNameString(n charsetName) string {
	if n == charsetASCII {
		return "ascii"
	}
	if n == charsetUK {
		return "uk"
	}
	return "special"
}

func checkSingleShift(t *testing.T, term *Terminal, want int) {
	t.Helper()
	if int(term.charsets.single) != want {
		t.Fatalf("single = %d want %d", int(term.charsets.single), want)
	}
}

func checkVisibleShape(t *testing.T, term *Terminal, visible bool, shape CursorShape) {
	t.Helper()
	got := term.Cursor()
	if got.Visible != visible {
		t.Fatalf("visible = %v want %v", got.Visible, visible)
	}
	if got.Shape != shape {
		t.Fatalf("shape = %d want %d", got.Shape, shape)
	}
}

func checkActiveScreen(t *testing.T, term *Terminal, want string) {
	t.Helper()
	got := "primary"
	if term.AlternateScreen() {
		got = "alternate"
	}
	if got != want {
		t.Fatalf("active = %q want %q", got, want)
	}
}

func checkSavedPos(t *testing.T, term *Terminal, x int, y int) {
	t.Helper()
	saved := savedCursorFor(term)
	if saved.x != x {
		t.Fatalf("saved column = %d want %d", saved.x, x)
	}
	if saved.y != y {
		t.Fatalf("saved row = %d want %d", saved.y, y)
	}
}

func savedCursorFor(term *Terminal) cursor {
	if term.AlternateScreen() {
		return term.screens[0].saved.cursor
	}
	return term.screen().saved.cursor
}

func checkSuffixLink(t *testing.T, term *Terminal, c continuationCase) {
	t.Helper()
	if c.suffixLink == "" {
		checkNoLink(t, term)
		return
	}
	checkHyperlink(t, term, c.suffixLink)
}

func checkNoLink(t *testing.T, term *Terminal) {
	t.Helper()
	if term.link != nil {
		t.Fatalf("link present")
	}
	checkPendingNoLink(t, term)
}

func checkPendingNoLink(t *testing.T, term *Terminal) {
	t.Helper()
	pending, ok := term.PendingCell()
	if !ok {
		return
	}
	if pending.Cell.Link != nil {
		t.Fatalf("pending link present")
	}
}

func checkHyperlink(t *testing.T, term *Terminal, want string) {
	t.Helper()
	if term.link == nil {
		t.Fatalf("link missing")
	}
	if term.link.URI != want {
		t.Fatalf("link mismatch")
	}
	checkHyperlinkCell(t, term, want)
	checkHyperlinkPending(t, term, want)
}

func checkHyperlinkCell(t *testing.T, term *Terminal, want string) {
	t.Helper()
	got := term.screens[0].rows[0].cells[2].Link
	if got == nil {
		t.Fatalf("cell link missing")
	}
	if got.URI != want {
		t.Fatalf("cell link mismatch")
	}
}

func checkHyperlinkPending(t *testing.T, term *Terminal, want string) {
	t.Helper()
	pending, ok := term.PendingCell()
	if !ok {
		t.Fatalf("pending missing")
	}
	if pending.Cell.Link == nil {
		t.Fatalf("pending link missing")
	}
	if pending.Cell.Link.URI != want {
		t.Fatalf("pending link mismatch")
	}
}

func TestContinuationInvalidFixtures(t *testing.T) {
	entries := listInvalidFixtures(t)
	for _, e := range entries {
		t.Run(e.Name(), func(t *testing.T) {
			checkInvalidFixture(t, e.Name())
		})
	}
}

func listInvalidFixtures(t *testing.T) []os.DirEntry {
	t.Helper()
	entries, err := os.ReadDir(filepath.Join("testdata", "invalid"))
	if err != nil {
		t.Fatalf("read invalid: %v", err)
	}
	if len(entries) == 0 {
		t.Fatalf("no invalid fixtures")
	}
	return entries
}

func checkInvalidFixture(t *testing.T, name string) {
	t.Helper()
	body, err := os.ReadFile(filepath.Join("testdata", "invalid", name))
	if err != nil {
		t.Fatalf("read %s: %v", name, err)
	}
	_, err = Restore(body)
	if errors.Is(err, ErrInvalidCheckpoint) {
		return
	}
	if errors.Is(err, ErrIncompatible) {
		return
	}
	t.Fatalf("Restore %s invalid", name)
}
