# markitdown (CLI)

Single self-contained binary (~6 MB, no runtime dependencies) converting
documents to Markdown. Flags mirror the Python `markitdown` CLI, plus parallel
batch mode.

```bash
cargo build --release -p markitdown-cli   # → ../target/release/markitdown
```

## Usage

```bash
markitdown report.pdf                      # stdout
markitdown report.pdf -o report.md        # file
cat report.pdf | markitdown -x pdf        # stdin with extension hint
markitdown doc.bin -m application/pdf     # mimetype hint
markitdown data.csv -c shift_jis          # charset hint
markitdown a.pdf b.docx c.html -O out/    # batch: all cores via rayon
markitdown https://en.wikipedia.org/wiki/Rust  # URLs convert directly
markitdown --list-formats
markitdown big-scan.pdf                   # auto (default): Python fallback for
                                          # OCR & co. when MARKITDOWN_PY_BIN is set
markitdown --engine rust file.docx        # pin pure Rust
markitdown --engine python file.pdf       # force full Python fidelity
markitdown -V huge.pdf                     # verbose: per-page % + timing on stderr
```

Name collisions in batch mode keep the original extension
(`test.docx` + `test.xlsx` → `test.docx.md`, `test.xlsx.md`).

## Verbose progress (`-V` / `--verbose`)

Heavy files (a 350 MB PDF) used to look like they might be hanging. `-V` streams
phase + **per-page percentage** to **stderr** (stdout stays clean Markdown), so
you can see it working — across both engines:

```
$ markitdown -V big.pdf
  [detect      ] input: application/pdf (356.0 MB), .pdf
  [pdf         ] extracting text from 1240 page(s)…
  [pdf       1%] page 13/1240
  [pdf       2%] page 25/1240
  ...
  [pdf     100%] page 1240/1240
  [done        ] converted to 4823191 chars in 38.4s
```

Percentage lines are throttled to whole-percent changes (so a 1000-page PDF
prints ~100 lines, not 1000). For the Python/auto engines it logs the
`delegating to the Python engine…` / `retrying via the Python engine…` phases.
Verbose is single-file only; batch mode keeps its per-file `ok:`/`FAILED:` lines.

## Man page

Generated at build time by `clap_mangen` and embedded in the binary:

```bash
markitdown --emit-man | mandoc | less                                  # view (macOS)
markitdown --emit-man | man -l -                                       # view (Linux)
markitdown --emit-man | sudo tee /usr/local/share/man/man1/markitdown.1 >/dev/null
man markitdown                                                          # after install
```

## Exit codes

`0` success · `1` any conversion/IO failure (batch: nonzero if *any* input
failed; per-file status lines go to stderr).

## Tests

`cargo test -p markitdown-cli` — unit tests plus integration tests that run
the real binary against the Python suite's fixtures (stdout, stdin hints,
`-o`, batch, man page, error paths).
