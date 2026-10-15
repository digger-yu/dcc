# Contributing to dcc

Short version: dcc is a research compiler that has to build a Linux kernel, so the bar is
"prove it from the artifact", not "looks right". One commit message convention, one gate list,
and no silent failure anywhere in the toolchain.

## 1. Gates that must stay green

```bash
cargo build --release
cargo build --tests
cargo test --release          # lib + every top-level test target
bash scripts/check_fixtures.sh             # every fixture is referenced and exists
bash scripts/check_fixtures_selftest.sh    # proves the check above can actually go red
```

`make test` runs that Tier-0 set; `make test-all` adds the wider suite (option-equivalence,
REX hygiene, pipe structure, disassembly coverage, cross golden files). CI jobs and the failure
triage order are in [docs/ci_guide.md](../docs/ci_guide.md).

## 2. The rule that shapes everything else

A capability may only be claimed if something can see it in the build output.

- Back-end features are declared in `FeatureSet` (`src/codegen/shared/features.rs`) and checked
  by `scripts/probe_backend_features.sh` against `docs/backend_feature_matrix.md`. If the probe
  and the declaration disagree, CI fails. Do not upgrade a `◐` (accept-level) cell to `✅`
  without an artifact-level judge.
- Nothing may fail silently. An option that cannot be honored warns or errors; a pass that no
  longer affects the output gets deleted, not left running. Precedent: the Steensgaard alias
  analysis, loop tiling/interchange as SSA passes and function attribute propagation were all
  removed because their results were discarded downstream.
- Optimiser output must stay deterministic, because self-hosting is verified by diffing objects
  (`make bootstrap`, `--bootstrap-compare`).

## 3. Tests

- Language and back-end cases live in `tests/integration/<theme>.rs`, pasted into the single
  `tests/integration.rs` target by `include!`; shared helpers go in `tests/common/harness.rs`,
  not in theme files.
- Every fix gets a fixture under `tests/fixtures/` plus an integration test that uses it.
- Diagnostics are snapshot-tested: `tests/fixtures/errors/<name>.{c,stderr.snap}`, refreshed
  with `DCC_UPDATE_SNAPSHOTS=1 cargo test --test errors --release`. Changing the output layout
  means bumping `DIAGNOSTIC_FORMAT_VERSION` in `tests/errors.rs`.
- After touching `tests/`, regenerate the index: `bash scripts/gen_coverage_index.sh`.
  `docs/test_coverage.md` is machine-generated; hand edits there get overwritten.
- Structure contracts (why each of the above is shaped that way): `tests/README.md`.

## 4. Debugging aids

| Knob | Use |
|---|---|
| `DCC_OPT_SKIP=<pass>[,...]` | disable one AST-layer pass |
| `DCC_SSA_OFF=<pass>` | disable one SSA pass (`gvn`, `sccp`, `dse`, `pipe`, `jt`, `sched`, …) |
| `DCC_SSA_IR_DUMP=1`, `DCC_SSA_BAIL=1` | dump the IR after the pipeline / log SSA bail-outs |
| `DCC_INLINE_TRACE=1` | log every inline decision and its reason |
| `DCC_NO_PARALLEL=1`, `-j1` | serialise codegen when comparing byte output |
| `--verbose` | phase report, linker path taken, LTO summary sizes |

Bisect with the two `*_OFF` switches before reading code; that is how most miscompiles here get
located.

## 5. Commit messages

- Subject: concise, imperative, optional `scope:` prefix (`x86_64/asm:`, `link:`, `docs:`).
- Body in English, explaining **what each touched module changed and why** — the root cause,
  not just the fix. For a miscompile, name the shape that triggered it.
- Every body line ≤ 72 characters.
- Sign off: `git commit -s` (one `Signed-off-by` trailer; DCO-style).
- Do not reformat unrelated code in a behavioral commit.

## 6. Documentation that moves with the code

| Change | Update |
|---|---|
| Any capability | `docs/support_features.md` or `docs/missing_features.md`, plus `docs/backend_feature_matrix.md` for back-end cells |
| New or changed diagnostic | `docs/diagnostics.md` (follow its "add a new code" checklist) |
| Language coverage | `docs/c_standard.md` |
| Milestone work | `docs/roadmap.md` |
| A decision with a rejected alternative | `docs/adr.md` (amend in place with a date; do not rewrite history) |

## 7. Build environment

A Rust 2021 toolchain **and** a host C compiler (`zstd-sys` compiles C). Optional, and only
needed to run the corresponding gates locally: `<triple>-as` for the assembly-parity job,
`qemu-system-x86_64` for the boot matrix, a Linux source tree for
`scripts/kernel_full_check.sh`, and a GCC checkout for the libiberty / cc1plus gates.

## 8. Getting in touch

Issues and patches are welcome. This is a personal research project: responses may be slow, and
a patch that adds a large dependency, a second code path for the same job, or an unverifiable
capability claim will likely be declined rather than merged.
