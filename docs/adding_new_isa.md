# Adding a New ISA Backend to dcc

This is the onboarding checklist for introducing a new codegen backend
on top of the shared layer in `src/codegen/shared/`. It assumes you
have already read `architecture.md` and `backend_feature_matrix.md`.

## 1. Register the ISA

* Extend `crate::cli::Arch` in `src/cli.rs` with the new variant.
* Extend `crate::cli::Os` / `crate::cli::Abi` if the target tuple
  needs a new combination (typical Linux cross triples reuse the
  existing `Os::Linux`).
* Extend `crate::codegen::shared::asm_source::AsmIsa` so the ISA has
  a typed identity (this enum replaces the legacy `arch: &str`
  stringly typed field).
* Pick a `DegradeGranularity`:
  * `PerStatement` for fixed-width ISAs (aarch64, arm, mips32/64).
  * `WholeTu` for variable-width ISAs (riscv64/32 with compressed
    16/32-bit encodings — per-statement degrade would corrupt stream
    boundaries).

## 2. Implement the Backend trait

Create `src/codegen/<arch>/backend.rs` with:

* `fn arch(&self) -> Arch`
* `fn features(&self) -> FeatureSet` — see "Declaring features"
  below. Do not simply call `FeatureSet::full()`; it would let the
  new backend falsely claim tier-1 capabilities it has not actually
  probed.
* The usual `compile` / `emit_*` methods that produce a
  `CommonImage` (the shared ELF assembler consumes this type).

Register the backend in `src/codegen/shared/registry.rs` so
`BackendRegistry::for_target` can hand it out.

## 3. Declaring features

The honest source of truth is `FeatureSet` in
`src/codegen/shared/features.rs`. Each item you list there must be
backed by a real artifact probe in `scripts/probe_backend_features.sh`
(see `backend_feature_matrix.md` for the probe-to-feature mapping).

Rules of thumb:

* Reuse `tier1_cross()` / `riscv64_cross()` / `tier2_cross()` only
  when the new backend's measured capability matches the ctor's
  current list.
* Add `Feature::GasTextOutput` only when the host actually has
  `<triple>-as` available and the `-S` output is accepted by that
  assembler. Missing this capability means `--asm-fallback=gas` will
  produce an honest E0312 error at the point of first degrade.

## 4. Inline asm pipeline

Inline asm goes through:

```
split_source_lines -> classify_line -> analyze / analyze_with
                   -> record_asm_stmt -> CommonImage.asm_passthrough
```

The backend must never silently encode an unknown instruction as a
NOP. If a template line is not encodable by the backend's encoder,
`record_asm_stmt` marks it passthrough; `codegen::emit()` then
decides (via `--asm-fallback`) whether to emit a hard E0311 or to
degrade the whole TU to GAS text.

## 5. Diagnostic codes

* `E0311` — inline asm template cannot be encoded (default policy:
  hard error).
* `E0312` — `--asm-fallback=gas` requested but the backend does not
  declare `Feature::GasTextOutput`, or the `<triple>-as` binary is
  missing at runtime.
* `E0101` / `E0102` — IO or assembler failure during the P7-3
  as-wiring step.

## 6. Tests

* Add unit tests under `src/codegen/<arch>/` for the encoder (freeze
  the expected bytes; see `tests/fixtures/asm/mini_asm_encode_freeze.txt`
  as the reference for what "frozen baseline" looks like).
* Add integration coverage under `tests/integration/c_asm.rs` for
  statement-position inline asm, cross objdump read-back, and the
  `--asm-fallback=gas` degrade path.
* Gate any test that requires `<triple>-as` behind a `which` probe
  and print `ASM-FALLBACK-SKIP: ...` on miss — never silently pass.

## 7. CI wiring

* Update `scripts/probe_backend_features.sh` so the feature matrix
  in CI matches your `FeatureSet::*()` declaration.
* Add the new target to `scripts/check_asm_text_parity.sh` so the
  G5 parity gate compares `-c` direct `.o` vs. `-S --asm-fallback=gas`
  `.o` on every fixture that exists for the new ISA.
