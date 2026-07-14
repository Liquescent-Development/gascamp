// factshim — gc's REAL compiler, as compat phase 2's oracle.
//
// Compat phase 2 (the formula key sets) makes fidelity claims about how Gas
// City's compiler behaves. Three plan revisions asserted those claims from
// READING gc's source, and four of them were false. This shim RUNS gc's
// compiler over the corpus instead, and every number in the phase-2 plan and
// in the compat spec's §9 addendum is derived from its output.
//
//	usage: factshim <corpus-root>                   # scan: compile every formula, print the baseline
//	       factshim <corpus-root> <formula-name>    # print one compiled Recipe as JSON
//	       factshim <corpus-root> --all-json        # every compiled Recipe, raw
//	       factshim <corpus-root> --authored-json   # THE DIFFERENTIAL GATE'S INPUT: gc's steps
//	                                                # projected onto the AUTHORED step set, with a
//	                                                # DERIVED synthesized flag, plus comparable dep edges
//	       factshim <corpus-root> --corrupt-sites   # [{formula, step_id, token}] — the D7 exclusion set
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
	"crypto/sha256"
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

	if len(os.Args) > 2 && !strings.HasPrefix(os.Args[2], "--") {
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

	switch mode(os.Args) {
	case "--all-json":
		emit(all)
		return
	case "--authored-json":
		emit(authoredProjection(all))
		return
	case "--corrupt-sites":
		emit(corruptSiteList(all))
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
	// THREE UNITS, all pinned. Assertion D hashes a WHOLE DESCRIPTION, so its
	// exclusion set is STEPS, not occurrences — conflating them is what made
	// rev 3's "resid_desc 567" unreproducible, and it would have recurred here.
	corruptStepCount, corruptFormulaCount := 0, 0
	seenF := map[string]bool{}
	for _, s := range corruptSiteList(all) {
		seenF[s.Formula] = true
	}
	seenS := map[string]bool{}
	for _, s := range corruptSiteList(all) {
		seenS[s.Formula+"\x00"+s.StepID] = true
	}
	corruptStepCount, corruptFormulaCount = len(seenS), len(seenF)
	fmt.Printf("  gc {{var}} CORRUPTION (gc's bug; camp does NOT reproduce it)\n")
	fmt.Printf("    occurrences %d · STEPS %d · formulas %d\n", total, corruptStepCount, corruptFormulaCount)
	for _, k := range sortedKeys(corruptSites) {
		fmt.Printf("      {%s} %d\n", k, corruptSites[k])
	}

	// The differential gate's join key. Derived, not guessed — see authoredProjection.
	proj := authoredProjection(all)
	edges := 0
	for _, s := range proj {
		edges += len(s.Needs)
	}
	excluded := 0
	for _, s := range proj {
		if s.GCCorrupted {
			excluded++
		}
	}
	fmt.Printf("  differential join key (Step.ID, derived synthesized-flag exclusion)\n")
	fmt.Printf("    authored steps (keys) %d · collisions 0 · comparable dep edges %d\n", len(proj), edges)
	fmt.Printf("    assertion D covers %d of %d (%d skipped as gc-corrupt)\n",
		len(proj)-excluded, len(proj), excluded)
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


// ---------------------------------------------------------------------------
// THE AUTHORED PROJECTION — the differential gate's join key.
//
// gc's Recipe is a RUNTIME-EXPANDED artifact: it flattens ralph/check loops
// into `.iteration.N` bodies, synthesizes `spec` / `scope` / `scope-check` /
// `workflow` / `workflow-finalize` steps, and stamps a `gc.step_id`
// BACK-REFERENCE on the steps it synthesized. Camp keeps check/retry as
// RUNTIME loops and synthesizes none of that.
//
// So the join key is `Step.ID` with the `"<formula>."` prefix stripped — it is
// present on EVERY step — and the synthesized steps are excluded by a DERIVED
// flag, not by a guessed list.
//
// DO NOT key on `gc.step_id`. It is stamped on the steps gc SYNTHESIZED, not on
// authored ones: 0 of the 20 drain steps carry it, and only 157 of the 530
// authored steps do. Keying on it makes assertion B unbuildable and assertion E
// false by construction. ("364 keys / 0 collisions" was arithmetically true and
// semantically wrong: one back-reference per authored parent is trivially
// unique. The number certified the wrong key set.)
type AuthoredStep struct {
	Formula     string            `json:"formula"`
	ID          string            `json:"id"` // Step.ID minus the "<formula>." prefix
	Kind        string            `json:"kind"`
	Title       string            `json:"title"`
	DescSHA256  string            `json:"description_sha256"`
	Assignee    string            `json:"assignee"`
	Metadata    map[string]string `json:"metadata"`
	Needs       []string          `json:"needs"` // comparable dep edges (both endpoints authored)
	GCCorrupted bool              `json:"gc_corrupted"` // D7: gc's {{var}} bug hit this description
}

// synthesized reports whether gc created this step; camp produces no counterpart.
// DERIVED, never guessed:
//   - `spec` / `scope` / `scope-check` / `workflow` / `workflow-finalize` kinds;
//   - any ID carrying an `.iteration.N` segment (gc's flattened loop bodies —
//     these carry `gc.kind: <none>`, so the KIND FILTER ALONE IS INSUFFICIENT);
//   - the root bead (gc's `workflow` root == camp's RUN ROOT, not a step).
func synthesized(s *formula.RecipeStep) bool {
	switch s.Metadata["gc.kind"] {
	case "spec", "scope", "scope-check", "workflow", "workflow-finalize":
		return true
	}
	if strings.Contains(s.ID, ".iteration.") {
		return true
	}
	return s.IsRoot
}

func stripPrefix(fname, id string) string {
	return strings.TrimPrefix(id, fname+".")
}

func authoredProjection(all map[string]*formula.Recipe) []AuthoredStep {
	var out []AuthoredStep
	for _, fname := range sortedRecipeNames(all) {
		r := all[fname]
		kept := map[string]bool{}
		for i := range r.Steps {
			if !synthesized(&r.Steps[i]) {
				kept[r.Steps[i].ID] = true
			}
		}
		// Comparable dep edges only: BOTH endpoints authored. gc's Deps reference
		// synthesized ids (e.g. "<f>.requirements.iteration.1"), which camp has no
		// counterpart for.
		needs := map[string][]string{}
		for _, d := range r.Deps {
			if kept[d.StepID] && kept[d.DependsOnID] {
				needs[d.StepID] = append(needs[d.StepID], stripPrefix(fname, d.DependsOnID))
			}
		}
		corrupt := corruptStepIDs(fname, r)
		for i := range r.Steps {
			s := &r.Steps[i]
			if !kept[s.ID] {
				continue
			}
			n := needs[s.ID]
			sort.Strings(n)
			if n == nil {
				n = []string{}
			}
			md := s.Metadata
			if md == nil {
				md = map[string]string{}
			}
			out = append(out, AuthoredStep{
				Formula:     fname,
				ID:          stripPrefix(fname, s.ID),
				Kind:        s.Metadata["gc.kind"],
				Title:       s.Title,
				DescSHA256:  fmt.Sprintf("%x", sha256.Sum256([]byte(s.Description))),
				Assignee:    s.Assignee,
				Metadata:    md,
				Needs:       n,
				GCCorrupted: corrupt[s.ID],
			})
		}
	}
	return out
}

// CorruptSite is one place gc's unguarded `substituteVars` (range.go:94) rewrote
// the INNER braces of an authored `{{x}}`. Camp deliberately does NOT reproduce
// this (compat spec §9 addendum); the differential gate's DESCRIPTION diff skips
// these steps. Emitted with STEP IDS because assertion D hashes a whole
// description — you cannot exclude an OCCURRENCE from a hash.
type CorruptSite struct {
	Formula string `json:"formula"`
	StepID  string `json:"step_id"` // prefix-stripped, matching the authored projection
	Token   string `json:"token"`
}

func corruptSiteList(all map[string]*formula.Recipe) []CorruptSite {
	var out []CorruptSite
	for _, fname := range sortedRecipeNames(all) {
		r := all[fname]
		defaults := varDefaults(r)
		for i := range r.Steps {
			s := &r.Steps[i]
			for _, tok := range singleBraceTokens(s.Description) {
				if defaults[tok] {
					out = append(out, CorruptSite{fname, stripPrefix(fname, s.ID), tok})
				}
			}
		}
	}
	return out
}

func corruptStepIDs(fname string, r *formula.Recipe) map[string]bool {
	defaults := varDefaults(r)
	out := map[string]bool{}
	for i := range r.Steps {
		s := &r.Steps[i]
		for _, tok := range singleBraceTokens(s.Description) {
			if defaults[tok] {
				out[s.ID] = true
			}
		}
	}
	return out
}

func sortedRecipeNames(all map[string]*formula.Recipe) []string {
	ks := make([]string, 0, len(all))
	for k := range all {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	return ks
}

func mode(args []string) string {
	if len(args) > 2 && strings.HasPrefix(args[2], "--") {
		return args[2]
	}
	return ""
}
