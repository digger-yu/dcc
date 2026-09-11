# Debug Session: torture ABI failures

Status: [OPEN]

## Scope

First target: `20020412-1.c` from the GCC torture Execute suite.
Baseline: 1697 total, 1664 passed, 33 failed, 0 skipped.

## Hypotheses

1. Caller and callee classify an aggregate inconsistently under SysV AMD64,
   causing a wrong register or stack slot.
2. `va_list` register-save or overflow-area initialization/progression is
   incorrect.
3. `va_start` preprocessing or parser metadata for named parameters is
   incorrect.
4. An implicit structure-return pointer changes the variadic argument layout.
5. The failure is caused by control flow or temporary-object reuse rather than
   ABI lowering.

## Evidence Plan

- Confirm GCC compilation and execution before DCC.
- Build and run the DCC and GCC versions separately at `-O0`.
- Save compiler stderr, exit status, signal/core information, and generated
  ELF inspection output under `/tmp/dcc-torture-20020412-1`.
- Minimize the reproducer without modifying the original torture source.
- Add only instrumentation before any logic fix.

## Findings

Pending baseline collection.

## Fix

Pending runtime evidence.

## Verification

Pending post-fix comparison.
