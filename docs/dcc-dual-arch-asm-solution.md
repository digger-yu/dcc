# dcc Dual-Arch ASM Solution

dcc compiles C for five architectures (x86_64, aarch64, riscv64,
arm, mips/mips64) from one shared codegen pipeline. This document
describes how inline assembly is represented, how it degrades when
a backend cannot encode a template, and how degraded builds still
produce linkable objects.

## 1. Two renderers, one image

`CommonImage` is the single artifact that leaves codegen. Two
renderers consume it:

* The **binary renderer** emits ELF bytes directly (the historical
  path for `-c`).
* The **GAS text renderer** emits `.s` text suitable for a real
  `<triple>-as` (the degrade path for `-S --asm-fallback=gas`).

Both renderers read the same `CommonImage` (including
`CommonImage.asm_passthrough`), so a TU is compiled **once** and two
textual/byte artifacts share the same instruction stream. This is
the determinism guarantee pinned by `asm_fallback_output_is_deterministic`.

## 2. Inline asm analysis pipeline

```
split_source_lines -> classify_line -> analyze / analyze_with
                   -> record_asm_stmt -> CommonImage.asm_passthrough
```

* `record_asm_stmt` records each template line verbatim.
* If the backend's encoder can produce a real instruction, it does
  so — the line never reaches `asm_passthrough`.
* If the backend cannot encode the line, **it must not NOP it**; it
  must mark the line as passthrough.
* The policy decision (hard error vs. degrade) is made in
  `codegen::emit()` based on `--asm-fallback`.

## 3. Fallback policies

`--asm-fallback=error` (default): emit `E0311` with the list of
passthrough lines. This preserves pre-P7 behavior for older build
scripts.

`--asm-fallback=gas`: the whole TU degrades to a GAS-text artifact.
The capability gate is `Feature::GasTextOutput` on the backend. If
the backend does not declare it, the fallback itself emits `E0312`
instead of writing an `.s` that cannot assemble.

## 4. Degrade granularity

`AsmIsa::degrade_granularity()` classifies ISAs into two buckets:

* `PerStatement` — fixed-width ISAs (aarch64, arm, mips32, mips64).
  A degrade can safely drop an instruction and still align
  subsequent streams.
* `WholeTu` — variable-width ISAs (riscv64/32 with 16/32-bit
  compressed). Splitting per statement would corrupt the compressed
  stream, so the entire TU degrades.

x86_64 falls into `WholeTu` in the current classification (its
variable-width instruction stream is treated the same way as RISC-V).

## 5. Output wiring (P7-3)

`write_emit_output` now branches on the requested output extension:

* If the user asked for `-S` (text), the degraded `.s` is written
  verbatim — no as invocation.
* If the user asked for `-c` (`.o`) **and** the backend declared
  `GasTextOutput`, dcc writes the GAS text to a temp `.s` and
  invokes `<triple>-as -o <requested> <tmp>`. Failures surface as
  `E0102` (assembler exit non-zero) or `E0312` (assembler binary
  missing). There is no silent fallback to a same-name `.s`.

`assembler_cmd_name` maps the target triple:

| Triple prefix | Command |
| ------------- | ------- |
| host (matches `TargetTriple::host()`) | `as` |
| `aarch64-linux-gnu` | `aarch64-linux-gnu-as` |
| `riscv64-linux-gnu` | `riscv64-linux-gnu-as` |
| `arm-linux-gnueabi` | `arm-linux-gnueabi-as` |
| `mips-linux-gnu` | `mips-linux-gnu-as` |
| `mips64-linux-gnu` | `mips64-linux-gnu-as` (expected to be absent) |
| anything else | `as` (intentional miss — let the OS say "not found") |

## 6. Tests and gates

* `tests/integration/c_asm.rs::asm_fallback_*` pins the policy
  matrix: default hard error, per-statement diagnostic grouping,
  `-S` degrade text, `-c` via as produces a real ELF, deterministic
  rendering, E0312 on mips64 (which does not declare
  `GasTextOutput`).
* `tests/integration/c_asm.rs::gas_fallback_produces_linkable_object`
  pins the P7-3 wiring for aarch64.
* `scripts/check_asm_text_parity.sh` compares `.text` bytes
  between direct `-c` and `-S --asm-fallback=gas` for every (fixture,
  arch) pair. Missing `<triple>-as` prints an explicit skip so CI
  never reports a silent green.

## 7. Known debt

* `mini_asm.rs` is retained as a thin encoder for inline asm
  operands; deleting it now would silently break the encoding path.
  See `backend_feature_matrix.md` for the debt note.
* mips64 does not have `GasTextOutput` because the host lacks
  `mips64-as`. Adding a cross binutils package is the unlock.
