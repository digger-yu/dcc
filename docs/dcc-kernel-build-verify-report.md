# DCC — Linux 7.1.0 (localmodconfig, x86-64): build & verification results

**Verdict (re-checked 2026-11-06, dcc `bebf30c7` + the follow-up frame-budget fix, QEMU 8.2.2).**
dcc builds the full `localmodconfig` x86-64 kernel **from a clean tree and a clean
include cache**: 2 901 translation units, **0 gcc/cc/clang trap hits and 0
`GCC-DELEGATE` lines**, a 171 212 384-byte `vmlinux` that **objtool accepts with 0
errors**, a `__jump_table` that decodes with **0 bad entries out of 5 053**, and realmode
assembly **byte-identical to gcc in all 6 comparisons**.

**The boot gate does not pass.** QEMU/PVH dies at 0.15 s with
`PANIC: early exception 0x0d … queued_spin_lock_slowpath+0xe17`, i.e. **before**
`Run /init as init process`. §5.11 isolates the cause to a real codegen defect — under
`-O2` dcc binds a named `"+m"` output to the frame slot of the *invisible* `"=@ccz"`
output in the same `asm`, so `arch_atomic_try_cmpxchg()` compares and writes the wrong
address and returns a garbage condition code — and §5.12 records a second kernel-class
defect (a fixed 300–400 B reserved frame per function, 10–40× gcc).

An earlier revision of this report claimed the boot gate passed. It was wrong, and the
reason is worth stating plainly: the `vmlinux` that reached `/init` had
`Linux version … (gcc 13.3.0 …)` in its `.rodata` — it was a **gcc-built** kernel. The
first dcc-only kernel is this one, and it does not boot yet.

This file records final results only: what was built, how it was verified, what this run
found, the reproduce script, and the open gaps.

---

## 1. Sources and environment

| Item | Value |
|---|---|
| dcc | this repository `bebf30c7`, `cargo build --release` → `target/release/dcc` (`dcc 0.1.0`) |
| Kernel | Linux 7.1.0, pristine source tree `/root/linux-pvh-src` (no git metadata, no local patches) |
| Config | `localmodconfig` seeded from `defconfig`, then `PVH=y`, `SUSPEND=n`, `HIBERNATION=n`, `ACPI_SLEEP=n`, `IA32_EMULATION=n`, `COMPAT=n` |
| Build dir | out-of-tree `O=/root/pvh-lmc`, **created empty**; include cache (`~/.dcc`) deleted first, so every unit was preprocessed cold |
| Host | Linux x86-64, 4 cores; `gcc 13.3.0` is used only as a comparison baseline, never as a build compiler |
| Emulator | QEMU 8.2.2 (`-kernel` PVH entry, `-m 1024M`, serial console) |

`.config` has 1 577 `=y` and 1 `=m` symbols; the build tree holds 2 998 object files. The
same `CC` runs at configure and build time, so `CONFIG_CC_VERSION_TEXT` stays
`dcc 0.1.0`. kbuild therefore also records `CONFIG_CC_IS_CLANG=y` (dcc predefines
`__clang__`); that is consistent rather than contradictory, because the clang-shaped
`-f…`/`-m…` flags kbuild then derives are exactly the ones on every dcc compile line, and
dcc documents which of them it implements (`docs/support_features.md`).

## 2. What “all-dcc” means here

The build is driven through `scripts/dcc-kernel-cc.sh` / `scripts/dcc-host-wrapper.sh`
(`CC=` / `HOSTCC=`), which hand every C/asm compile to `target/release/dcc`. Two
independent checks make the claim falsifiable:

* `scripts/kernel_pvh_verify.sh --build` prepends a trap directory to `PATH` containing
  `gcc`, `cc`, `g++`, `c++`, `clang`, `ld.bfd`; each trap appends its command line to a
  log and exits 99. **0 trap hits** for this build.
* the same script counts `GCC-DELEGATE` lines in the build log (the wrapper used to fall
  back to gcc for a few infrastructure files). **0 delegate lines.**

Host `ld` still does the final link — dcc emits relocatable objects and drives the system
linker directly; it is not a linker. That is the same split as a gcc build
(`gcc` → `as` → `ld`), with dcc replacing `cc` and `as` for the compilation steps.

## 3. Build procedure

```bash
# configuration (O= out-of-tree; same CC at configure and build time)
KERNEL_TREE=/root/linux-pvh-src BUILD=/root/pvh-lmc \
  bash scripts/kernel_pvh_verify.sh --config
# build (CC/HOSTCC = dcc wrappers, -j2)
KERNEL_TREE=/root/linux-pvh-src BUILD=/root/pvh-lmc DCC_BIN=target/release/dcc JOBS=2 \
  bash scripts/kernel_pvh_verify.sh --build
```

Measured for this report, on the cold-cache clean build:

| Metric | Value |
|---|---|
| `CC` lines in the build log | 2 901 |
| object files in the tree | 2 998 |
| wall time, `-j2` on 4 cores | 3 h 52 min |
| `vmlinux` | 171 212 384 B |
| gcc traps / delegate lines | 0 / 0 |
| objtool errors | 0 |

`make vmlinux` does not build `arch/x86/boot/bzImage`; the PVH route uses `vmlinux`
directly, so no bzImage is produced by this recipe.

## 4. Verification results

### 4.1 objtool

objtool runs as part of the kbuild link path for every object in this configuration.
Result: **0 errors**. The only diagnostics are `return with modified stack frame`
warnings (**12 225** occurrences across the whole tree); they are warnings, not errors,
and objtool and the link both exit 0. They are not evidence of correctness either way,
but they are the reason `vmlinux` links at all: objtool's ORC pass does accept dcc's
prologues/epilogues.

### 4.2 ELF-level checks

* `__jump_table`: **5 053 entries**; each entry's `code` target must point at the first
  byte of a `jmp` (`0xeb`/`0xe9`). **`bad_nop=0`, `bad_other=0`, `out_of_text=0`** — no
  entry points into a `.text` NOP hole left behind by a moved function.
* `.altinstructions`: **2 349 entries** (`0x8076` bytes, 14 B each); the nested
  `ALTERNATIVE_2` sites in `arch/x86/kernel/pvclock.o` carry the GAS-compatible
  `instrlen/repllen` pairs `(5,5)` and `(5,3)`, and `.altinstr_replacement` contains the
  real `lfence; rdtsc; rdtscp` bytes rather than NOPs.
* executable `.pushsection` blocks (`.altinstr_aux`, `.altinstr_replacement`, `.fixup`,
  `.ex_table`) carry their own `R_X86_64_PC32` relocations in `.rela.<section>`:
  cross-section jumps are emitted against the **target section symbol** with
  `addend = offset - 4`, matching what GAS produces for the same inline asm.

### 4.3 Boot (QEMU PVH) — **FAIL**

```bash
KERNEL_TREE=/root/linux-pvh-src BUILD=/root/pvh-lmc \
  bash scripts/kernel_pvh_verify.sh --boot
```

```
[    0.148092] ACPI: Error parsing LAPIC address override entry
[    0.148350] ACPI: Invalid BIOS MADT, disabling ACPI
[    0.148633] Intel MultiProcessor Specification v1.4
[    0.152984] No NUMA configuration found
PANIC: early exception 0x0d IP 10:ffffffff87320227 error 0 cr2 0xf9de3ff183e9a89b
[    0.160025] RIP: 0010:queued_spin_lock_slowpath+0xe17/0x1340
[    0.161840] Code: … 49 8b 45 a0 <48> 89 01 …        # mov %rax,(%rcx)
```

The harness's own line is `FAIL: PVH 启动未到用户空间`; neither
`Run /init as init process` nor `DCC kernel: init reached successfully` appears.

Two things were ruled out before the cause was found (§5.11):

* **not a stack overflow.** A hardware breakpoint on the faulting instruction reports
  `rsp = init_thread_union+4728` — 4.6 KB of the 16 KB thread stack in use.
* **not an objtool/link problem.** Everything in §4.1/§4.2 is clean; the kernel loads,
  decompresses and reaches `start_kernel`.

### 4.4 Realmode assembly (`--partial`)

The six `.S` files that end up in `rmpiggy.o` are assembled by dcc with `-m16` and
compared against `gcc -m16 -c` after `objcopy --strip-debug`: code, relocations and
symbols are **byte-identical** (6/6). The `-m16` **C** inputs still fail with a hard
`E0319` diagnostic plus a `DCC-GAP reason=m16/m32-no-c-backend` marker instead of
silently emitting 64-bit code, and the gcc trap stays empty (14 passed, 0 failed).
This stage has to run after `--build`: the six files include build-generated headers
(`generated/autoconf.h`, `generated/asm-offsets.h`).

### 4.5 dcc's own test suite

`cargo test --release --no-fail-fast -- --test-threads=2`: **4 042 passed, 8 ignored,
1 failed**. The single failure is `cpp_abi_eh_group` (six C++ zero-cost-EH fixtures crash
with a signal); it is pre-existing and unrelated — the same six fixtures failed the same
way in `logs/cargo_test_fix6.log` dated 2026-11-04, before any change in this cycle, and
C++ EH is not on the kernel path. `scripts/torture_test.sh all` (the pre-commit gate)
reports **3 707 passed, 0 failed, 6 skipped**.

## 5. Defects found in this cycle

### 5.1–5.10 fixed, each with a regression case

| # | Symptom | Root cause | Fix |
|---|---|---|---|
| 1 | `#pragma pack` state lost when a header came from the include cache | cache key did not cover/restore pack state | cache V9 + state replay; cold/hot two-pass `sizeof` case |
| 2 | `__jump_table` entries pointed into `.text` NOP holes | labels in functions moved to custom sections were still resolved against `.text` | label sectioning via `pushsection_code_ranges` / `custom_func_move_ranges` (`asm_label_in_moved_section_function_resolves_to_target_section`) |
| 3 | `objtool: weirdly overlapping alternative! 8 != 6` (`pvclock.o`) | nested `ALTERNATIVE_2`: the `.skip` measurement pass produced a stale coordinate (2 instead of 5) | `eval_asm_label_expr` prefers the current pass's coordinate; the measurement table is only a forward-reference fallback |
| 4 | `.altinstr_replacement` contained only NOPs | whole functions (source filled with `0x90`) were resolved before executable `.pushsection` blocks | move executable `.pushsection` ranges first, then functions |
| 5 | `objtool: can't find jump dest instruction at .altinstr_aux+0x2c` | a function range and a nested `.altinstr_aux` range both contained the jump; first match won | pick the **innermost** containing range |
| 6 | cross-section jumps inside moved blocks had no relocation | the symbol was the `.L` label, which is not in `.symtab` | emit `R_X86_64_PC32` against the target **section symbol**, `addend = offset - 4` (`cross_section_jump_out_of_exec_pushsection_carries_reloc`) |
| 7 | pushsection relocations landed in `.rela.init.text` at NOP-hole offsets | `emit_rel_relocations` used the same first-match rule | same innermost-range rule when migrating relocation offsets |
| 8 | `testb $1, boot_cpu_data+N(%rip)` read the neighbouring capability byte | PC32 addend was `-4`, but the immediate follows `disp32`, so RIP is `P+5` | `addend = -4 - imm_size` (`-5` for `testb`, `-8` for `testl`) |
| 9 | PVH boot died at `native_apic_mem_read`, APIC fixmap PTE `0xFFFFFFFF87DFF398` | a block-scope 8-byte struct initialised from a compound literal stored the **buffer address** as the value | `needs_compound_copy` in all three declaration paths (`struct_small_compound_init_in_block`) |
| 10 | `complex-7.c` fails to compile with `E1128 frame budget exhausted` once the constant 512 B vector band is dropped | `sret_bytes_expr` never registered the 32-byte `_Complex long double` materialisation slots, and summed `Binop` subexpressions as a maximum while the emitter bumps without reclaiming | `complex_ld_slot` + operand/argument/leaf accounting; `Expr::Binop` sums both sides |

### 5.11 New, **not yet fixed**: `"+m"` output bound to the `"=@ccz"` slot at `-O2`

**Reproducer** (`scripts/kernel_cc_asm_repro.sh`, copied from
`arch/x86/include/asm/rmwcc.h`'s `__GEN_RMWcc`, where the condition-code output is
*invisible* — the template never mentions it):

```c
static inline bool try_cmpxchg(int *v, int old, int new)
{
	bool c;
	asm volatile("lock cmpxchgl %[new], %[var]"
		     : [var] "+m" (*v), "=@ccz" (c)
		     : [new] "r" (new), "a" (old) : "memory");
	return c;
}
```

| | `-O0` | `-O2` |
|---|---|---|
| gcc | `fail=0 ok=1 v=3` | `fail=0 ok=1 v=3` |
| dcc | `fail=0 ok=1 v=3` | **`fail=0 ok=0 v=5`** (rc 1) |

dcc `-O2` emits

```
lock cmpxchgl %rdx, -64(%r13)     # %[var] resolved to -64(%r13) …
movzbl  -64(%r13), %eax           # … which is c's own frame slot
```

so the compare-and-swap targets the *condition code variable* instead of `*v`: `v` is
never written and `c` is read back as whatever the swap left behind. Nothing warns —
the operand map is simply wrong.

This is exactly the shape inside `arch_atomic_try_cmpxchg()` / `xchg()` in
`kernel/locking/qspinlock.o`, and it explains the boot failure end to end:
`queued_spin_lock_slowpath()` gets a garbage `prev` from `xchg_tail()`, skips its
`if (prev & _Q_TAIL_MASK)` guard, and stores through it
(`mov %rax,(%rcx)`, `rcx = 0xf9de3ff183e9a89b`) → `#GP` → `#DF` → early panic. The
generated `.s` of the kernel object shows the same missing write-back.

Status: root-caused, fix not attempted in this cycle. The narrow defect is the operand
binding of a named memory output when an invisible `@cc` output shares the same `asm`;
the wider lesson is that a mis-bound operand is silent, so the binding path needs a
fail-closed check (an output whose address slot collides with another output's slot must
be an error, not codegen).

### 5.12 New, **not yet fixed**: a fixed 300–400 B reserved frame per function

Same 120 objects (`kernel/`, `mm/`, `lib/`), prologue `sub rsp` per function:

| compiler | most common prologue frames |
|---|---|
| gcc | `0x8`, `0x10`, `0x18`, `0x20`, `0x28`, `0x30` |
| dcc | `0x140`, `0x150`, `0x160`, `0x170`, `0x180`, `0x190` |

`queued_spin_lock_slowpath()` gets `sub rsp,0x920` (2 336 B) from dcc; gcc's version of
the same function has **no** `sub rsp` at all. The frame is a sum of bands
(`abi/layout.rs::LayoutMap::resolve`), and several of them are still reserved
unconditionally: `FIXED_HDR` 48, `CX_FRAME_BAND` 48, `BF_RMW_RESERVE` 64,
`stack_reserve` 128 — 288 B before the function holds anything.

Two of the three historical offenders are already on demand: the 512 B vector band is
reserved only when the scan finds a vector type (`vec_band_reserve(0) == 0`, which took a
flat function from 912 B to 352 B), and the trampoline band is now
`nested_count × 32` instead of a flat 256. The complex band, the bitfield-RMW band and
the 128 B floor are still flat.

On a 16 KB kernel thread stack this is a live hazard even though it is **not** what
crashed this boot (§4.3 measured 4.6 KB used at the fault): 288 B of unavoidable frame
per level means the deepest early-boot chains have only ~55 frames of headroom, and any
local array can push a caller past its own frame into the next. Fixing it is a
layout-level change and needs a full-tree rebuild to prove, so it is recorded rather than
attempted here.

## 6. Reproduce

```bash
DCC=/root/dcc/target/release/dcc
TREE=/root/linux-pvh-src
BUILD=/root/pvh-lmc

# 0. dcc itself
cd /root/dcc && cargo build --release -j2

# 1. configuration: localmodconfig + PVH, no 16/32-bit C paths
KERNEL_TREE=$TREE BUILD=$BUILD DCC_BIN=$DCC \
  bash scripts/kernel_pvh_verify.sh --config

# 2. full vmlinux build with gcc traps armed (expect: traps 0, delegates 0)
KERNEL_TREE=$TREE BUILD=$BUILD DCC_BIN=$DCC JOBS=2 \
  bash scripts/kernel_pvh_verify.sh --build

# 3. realmode .S byte-for-byte vs gcc (expect: 14 passed, 0 failed; needs step 2's headers)
KERNEL_TREE=$TREE BUILD=$BUILD DCC_BIN=$DCC \
  bash scripts/kernel_pvh_verify.sh --partial

# 4. ELF checks
bash scripts/kernel_scan_jump_table.sh "$BUILD/vmlinux"   # expect: ok=5053 bad_nop=0
readelf -SW "$BUILD/vmlinux" | grep -E 'altinstr|jump_table'

# 5. boot — currently expected to FAIL at ~0.15 s (§4.3, §5.11)
KERNEL_TREE=$TREE BUILD=$BUILD bash scripts/kernel_pvh_verify.sh --boot

# 6. the §5.11 reproducer as a check (gcc here is a comparison baseline, not a build tool)
bash scripts/kernel_cc_asm_repro.sh
#   gcc -O2 : fail=0 ok=1 v=3 rc=0
#   dcc -O2 : fail=0 ok=0 v=5 rc=1        → RESULT: FAIL until §5.11 is fixed

# 7. the frame histogram of §5.12 (same 120 objects, prologue `sub rsp` only)
bash scripts/kernel_frame_hist.sh /root/pvh-lmc      # 0x140…0x1b0 dominate
bash scripts/kernel_frame_hist.sh /root/pvh-gcc-ref  # 0x8…0x50 dominate

# 8. regression gates
cd /root/dcc
cargo test --release --no-fail-fast -- --test-threads=2   # 1 known C++ EH failure
JOBS=4 bash scripts/torture_test.sh all                   # expect: fail=0
```

## 7. What is not verified / known gaps

* **Boot to userspace is not achieved** by a dcc-only kernel (§4.3). Until §5.11 is
  fixed, no dcc-built `vmlinux` in this configuration can reach `/init`.
* **`bzImage` / 16-bit C**: `arch/x86/boot/**` and `realmode/rm/{wakemain,video-mode}.c`
  are 16-bit C and need a backend dcc does not have (`E0319` + `DCC-GAP`, asserted in
  §4.4). The PVH route bypasses them; the remaining 16-bit material is assembly and is
  byte-identical to gcc.
* **`COMPAT` / `vdso32`**: `-m32` C, switched off in this configuration.
* **Modules**: this `localmodconfig` resolves to 1 module and `make vmlinux` does not run
  `modpost` or module linking.
* **C++**: `cpp_abi_eh_group` (six zero-cost-EH fixtures) still dies with a signal; it
  predates this cycle (§4.5) and is not on the kernel path.
* **Frame economy**: §5.12 is measured, not fixed; the 300–400 B floor plus the flat
  complex/bitfield bands are known to remain.
* **Silent mis-binding is unguarded**: §5.11 shows an operand binding can be wrong with
  no diagnostic. A fail-closed check on output-slot collisions is the follow-up that would
  turn this class of bug into a compile error.
