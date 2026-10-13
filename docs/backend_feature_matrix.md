# dcc Backend Feature Matrix

Honest capability grid. Every row claims a feature only when there
is a real artifact probe in `scripts/probe_backend_features.sh`
whose output can be matched against this table. If the probe and the
Rust `FeatureSet::*()` declaration disagree, CI turns red.

Legend:

* `✅` — artifact-probed: ELF section, UND symbol, or bytes match.
* `◐` — accept-level only: dcc accepts the construct and emits
  non-empty `.text`, but no artifact-level proof yet. Do **not**
  promote CI gates on this cell.
* `❌` — explicitly probed and rejected (with the specific error
  captured in the probe script).
* `?` — not yet probed; do not rely on it.

## Host backends

| Feature | x86_64-linux-gnu |
| ------- | --------------- |
| `CoreArith` / `CoreCall` | ✅ |
| `Float` / `LongDouble` / `Int128` | ✅ |
| `Bitfield` / `VaList` | ✅ |
| `Tls` / `Atomics` | ✅ |
| `InlineAsm(Full)` | ✅ |
| `Vector` | ✅ |
| `Sanitizer(Address)` / `Sanitizer(UndefinedBehavior)` | ✅ |
| `CfiUnwind` / `StackProtect` | ✅ |
| `ParallelCodegen` | ✅ |
| `Lto` / `BoundsCheck` / `Trapv` | ✅ |
| `ObjectFormat(Elf)` / `ObjectFormat(Coff)` | ✅ |
| `GasTextOutput` | ✅ (host `as` available) |
| `Dwarf(4)` | ✅ |

## Cross backends (tier 1)

| Feature | aarch64-linux-gnu | riscv64-linux-gnu |
| ------- | ----------------: | ----------------: |
| `CoreArith` | ✅ | ✅ |
| `CoreCall` | ❌ (declared missing — probe captures no UND callee) | ❌ |
| `Float` | ✅ | ✅ |
| `LongDouble` | ✅ | ❌ (Unary(Neg) on long double fails) |
| `Int128` | ✅ | ✅ |
| `Bitfield` / `VaList` | ✅ | ✅ |
| `Atomics` / `Vector` | ◐ | ◐ |
| `InlineAsm(Partial)` | ◐ | ◐ |
| `CfiUnwind` | ✅ (`.eh_frame`) | ✅ |
| `StackProtect` | ❌ (Assign lowering fails) | ❌ |
| `ObjectFormat(Elf)` | ✅ | ✅ |
| `Dwarf(4)` | ✅ | ✅ |
| `Tls` / `Lto` | ❌ | ❌ |
| `Sanitizer(*)` | ❌ | ❌ |
| `RiscVBorrowedRegs` | n/a | ✅ |
| `GasTextOutput` | ✅ (aarch64-linux-gnu-as installed) | ✅ (riscv64-linux-gnu-as installed) |

## Cross backends (tier 2)

The tier-2 set is a shared ctor; arm / mips32 / mips64 share the
**intersection** of their measured capabilities. Individual backends
override with `with_gas_text_output()` etc.

| Feature | arm-linux-gnueabi | mips-linux-gnu | mips64-linux-gnu |
| ------- | ----------------: | -------------: | ----------------: |
| `CoreArith` / `Float` / `Bitfield` / `VaList` | ✅ | ✅ | ✅ |
| `CoreCall` | ❌ | ❌ | ❌ |
| `InlineAsm(Minimal)` | ◐ | ◐ | ◐ |
| `CfiUnwind` | ✅ | ✅ | ✅ |
| `StackProtect` | ✅ | ✅ | ✅ |
| `ObjectFormat(Elf)` | ✅ | ✅ | ✅ (see ordering caveat below) |
| `Dwarf(4)` | ✅ | ✅ | ✅ |
| `GasTextOutput` | ✅ (arm-linux-gnueabi-as) | ✅ (mips-linux-gnu-as) | ❌ (no mips64-as on CI hosts) |

### mips64 known caveats

* ELF64 container used to have endian inconsistency
  (`e_ident` MSB vs. header LSB); `elf.rs::build_elf_rel_generic`
  now honors the file's own `EI_DATA`, so the container is accepted
  by `objdump -h`, but the symtab still shifts 25 bytes for the
  first empty symbol on ELF64 MSB — treat any UND-symbol-based probe
  on mips64 as `?` until that defect is fixed.
* No `mips64-*-as` is available on the host, so `GasTextOutput` is
  deliberately **not** declared. If you install the toolchain, flip
  `src/codegen/mips/mips64.rs::features()` to
  `FeatureSet::tier2_cross().with_gas_text_output()` and update this
  row.

## Tech debt

* `src/codegen/shared/mini_asm.rs` still exists. It encodes a small
  subset of inline asm operands and is the source of truth for the
  inline-asm encoding path consumed by `asm_source.rs:342-346`.
  Deleting it now would silently break inline asm encoding across
  every backend. Remove only after the per-backend inline asm
  pipelines have absorbed its responsibilities.
* The tier-2 ctor is an intersection for three backends. As
  mips64 diverges, consider splitting it.

## Probes

* `scripts/probe_backend_features.sh` reads every `✅` cell above and
  fails CI when an artifact does not match the declared feature.
* `scripts/check_asm_text_parity.sh` verifies `-c` vs.
  `-S --asm-fallback=gas` produces matching `.text` bytes.
