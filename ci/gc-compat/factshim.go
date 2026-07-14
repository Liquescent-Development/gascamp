// factshim — gc's REAL compiler, as compat phase 2's oracle.
//
// Compat phase 2 (the formula key sets) makes fidelity claims about how Gas
// City's compiler behaves. Three plan revisions asserted those claims from
// READING gc's source, and four of them were false. This shim RUNS gc's
// compiler over the corpus instead, and every number in the phase-2 plan and
// in the compat spec's §9 addendum is derived from its output.
//
//	usage: factshim <corpus-root>                 # scan: compile every formula, print the baseline
//	       factshim <corpus-root> <formula-name>  # print one compiled Recipe as JSON
//	       factshim <corpus-root> --all-json      # print every compiled Recipe as JSON (the differential gate's input)
//
// It is built INSIDE a gascity checkout at ci/gc-compat/GASCITY_REF (the same
// way ci/gc-compat/camp_corpus_validate.go is), because it links gc's internal
// formula package:
//
//	mkdir -p gascity-src/cmd/factshim
//	cp ci/gc-compat/factshim.go gascity-src/cmd/factshim/main.go
//	cd gascity-src && go build -o /tmp/factshim ./cmd/factshim
//
// ALL formula dirs are used as search paths. With fewer, cross-pack `extends`
// cannot resolve and 33 of the 100 formulas fail to compile — every downstream
// number would then be wrong.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"github.com/gastownhall/gascity/internal/formula"
)

// A {{var}} placeholder that SURVIVES compilation. gc substitutes these at
// instantiation (stepToBead), never at compile — the single fact that most of
// phase 2's staging depends on.
var doubleBrace = regexp.MustCompile(`\{\{[A-Za-z_]\w*\}\}`)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: factshim <corpus-root> [<formula-name> | --all-json]")
		os.Exit(2)
	}
	root := os.Args[1]

	layers := formulaDirs(root)
	names := formulaNames(layers)

	if len(os.Args) > 2 && os.Args[2] != "--all-json" {
		r, err := formula.CompileWithoutRuntimeVarValidation(context.Background(), os.Args[2], layers, nil)
		if err != nil {
			fmt.Fprintln(os.Stderr, "FAIL:", err)
			os.Exit(1)
		}
		emit(r)
		return
	}

	all := map[string]*formula.Recipe{}
	var failed []string
	for _, n := range names {
		r, err := formula.CompileWithoutRuntimeVarValidation(context.Background(), n, layers, nil)
		if err != nil {
			failed = append(failed, fmt.Sprintf("FAIL %s: %v", n, err))
			continue
		}
		all[n] = r
	}

	if len(os.Args) > 2 && os.Args[2] == "--all-json" {
		emit(all)
		return
	}

	for _, f := range failed {
		fmt.Println(f)
	}
	summary(layers, names, all, failed)
}

func formulaDirs(root string) []string {
	var layers []string
	_ = filepath.Walk(root, func(p string, fi os.FileInfo, err error) error {
		if err != nil {
			return nil //nolint:nilerr // an unreadable dir is not a formula dir
		}
		if fi.IsDir() && fi.Name() == "formulas" && !strings.Contains(p, string(filepath.Separator)+".git"+string(filepath.Separator)) {
			layers = append(layers, p)
		}
		return nil
	})
	sort.Strings(layers)
	return layers
}

// Glob `formulas/*.toml`, NOT `*.formula.toml`: gastown's 8 `mol-*.toml` break
// the naming convention, and the narrow glob silently yields 92 formulas
// instead of 100.
func formulaNames(layers []string) []string {
	var names []string
	for _, l := range layers {
		fs, _ := filepath.Glob(filepath.Join(l, "*.toml"))
		for _, f := range fs {
			n := strings.TrimSuffix(filepath.Base(f), ".toml")
			names = append(names, strings.TrimSuffix(n, ".formula"))
		}
	}
	sort.Strings(names)
	return names
}

func emit(v any) {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		fmt.Fprintln(os.Stderr, "marshal:", err)
		os.Exit(1)
	}
	fmt.Println(string(b))
}

// summary prints the baseline the phase-2 plan pins. Every metric names its
// counting rule, because an ambiguous one invites tuning the shim until it
// prints the expected number.
func summary(layers, names []string, all map[string]*formula.Recipe, failed []string) {
	var (
		steps int
		// residDescSteps: STEPS whose Description holds >= 1 surviving {{var}}.
		// (The occurrence count is a different, larger number — 2396 — and the
		// two must never be confused.)
		residDescSteps  int
		residDescOccs   int
		residTitleSteps int
		residMeta       = map[string]int{}
		drainByContext  = map[string]int{}
		drainSteps      int
		kinds           = map[string]int{}
		// corruptSites: gc's OWN BUG. substituteVars (range.go:94) is an
		// unguarded ReplaceAllStringFunc over `\{(\w+)\}`, so inside expandStep
		// it matches the INNER `{x}` of an authored `{{x}}` and substitutes it.
		// gc's residual CHECKER carries the guard (parser.go:664-672); its
		// MUTATOR does not. Camp deliberately does NOT reproduce this — see the
		// compat spec §9 addendum. These are the sites the differential gate
		// must exclude from its description diff.
		corruptSites = map[string]int{}
	)

	for _, n := range names {
		r := all[n]
		if r == nil {
			continue
		}
		defaults := varDefaults(r)
		for _, s := range r.Steps {
			steps++
			if m := doubleBrace.FindAllString(s.Description, -1); len(m) > 0 {
				residDescSteps++
				residDescOccs += len(m)
			}
			if doubleBrace.MatchString(s.Title) {
				residTitleSteps++
			}
			kind := s.Metadata["gc.kind"]
			if kind == "" {
				kind = "<none>"
			}
			kinds[kind]++
			if s.Metadata["gc.kind"] == "drain" {
				drainSteps++
				drainByContext[s.Metadata["gc.drain_context"]]++
			}
			for k, v := range s.Metadata {
				if doubleBrace.MatchString(v) {
					residMeta[k]++
				}
			}
			for _, tok := range singleBraceTokens(s.Description) {
				if defaults[tok] {
					corruptSites[tok]++
				}
			}
		}
	}

	fmt.Printf("\nlayers=%d formulas=%d OK=%d FAIL=%d\n", len(layers), len(names), len(all), len(failed))
	fmt.Printf("  steps (compiled)                    %d\n", steps)
	fmt.Printf("  drain steps                         %d\n", drainSteps)
	for _, k := range sortedKeys(drainByContext) {
		fmt.Printf("    context=%-24s %d\n", k, drainByContext[k])
	}
	fmt.Printf("  resid_desc_steps  (STEPS with >=1 {{var}} in Description)   %d\n", residDescSteps)
	fmt.Printf("  resid_desc_occs   (OCCURRENCES of {{var}} in Descriptions)  %d\n", residDescOccs)
	fmt.Printf("  resid_title_steps (STEPS with >=1 {{var}} in Title)         %d\n", residTitleSteps)
	for _, k := range sortedKeys(residMeta) {
		fmt.Printf("  resid_meta[%-28s] %d\n", k, residMeta[k])
	}
	fmt.Printf("  gc.kind vocabulary:\n")
	for _, k := range sortedKeys(kinds) {
		fmt.Printf("    %-20s %d\n", k, kinds[k])
	}
	total := 0
	for _, v := range corruptSites {
		total += v
	}
	fmt.Printf("  gc {{var}} CORRUPTION sites (gc's bug; camp does NOT reproduce it)  %d\n", total)
	for _, k := range sortedKeys(corruptSites) {
		fmt.Printf("    {%s} %d\n", k, corruptSites[k])
	}
}

// varDefaults returns the set of VALUES a formula's vars default to. A
// single-brace token in a compiled Description whose text equals one of these
// is a corruption site: there is no var named "superpowers.implementer" — that
// string is the VALUE that gc substituted into the inner braces of an authored
// `{{implementation_target}}`.
func varDefaults(r *formula.Recipe) map[string]bool {
	out := map[string]bool{}
	for _, vd := range r.Vars {
		if vd != nil && vd.Default != nil && *vd.Default != "" {
			out[*vd.Default] = true
		}
	}
	return out
}

var singleBrace = regexp.MustCompile(`\{([A-Za-z_][\w.\-]*)\}`)

// singleBraceTokens returns `{tok}` occurrences that are NOT part of a `{{tok}}`
// — the same guard gc's residual checker carries (parser.go:664-672) and its
// mutator does not.
func singleBraceTokens(s string) []string {
	var out []string
	for _, m := range singleBrace.FindAllStringSubmatchIndex(s, -1) {
		start, end := m[0], m[1]
		if start > 0 && s[start-1] == '{' {
			continue
		}
		if end < len(s) && s[end] == '}' {
			continue
		}
		out = append(out, s[m[2]:m[3]])
	}
	return out
}

func sortedKeys(m map[string]int) []string {
	ks := make([]string, 0, len(m))
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	return ks
}
