// Validates that every formula in a directory compiles under the real Gas
// City formula-v2 compiler. Lives in gascamp; runs inside a gascity checkout.
package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/gastownhall/gascity/internal/formula"
)

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: camp-corpus-validate <formula-dir>")
		os.Exit(2)
	}
	files, err := filepath.Glob(filepath.Join(os.Args[1], "*.toml"))
	if err != nil || len(files) == 0 {
		fmt.Fprintf(os.Stderr, "no formulas found in %s (err=%v)\n", os.Args[1], err)
		os.Exit(2)
	}
	failed := 0
	for _, path := range files {
		name := strings.TrimSuffix(filepath.Base(path), ".toml")
		if _, err := formula.CompileWithoutRuntimeVarValidation(
			context.Background(), name, []string{os.Args[1]}, nil); err != nil {
			fmt.Fprintf(os.Stderr, "FAIL %s: %v\n", name, err)
			failed++
			continue
		}
		fmt.Printf("OK   %s\n", name)
	}
	if failed > 0 {
		os.Exit(1)
	}
}
