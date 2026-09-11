# Debug va_list and aggregate ABI

Status: [OPEN]

## Scope

First debugging session for the remaining torture Execute failures.
Focus only on va_list and aggregate ABI behavior.

Initial failures:

- 20020412-1.c
- va-arg-pack-1.c
- pr117432.c (previously compile-failed; runtime status must be rechecked)

## Falsifiable hypotheses

1. va_list initialization or advancement reads the wrong register-save or
   overflow area address.
2. Aggregate argument classification differs between caller and callee.
3. long double or highly aligned arguments break overflow-area alignment.
4. __builtin_va_arg_pack loses wrapper variadic arguments.
5. The failure is caused by control flow or temporary-slot reuse rather than
   argument passing.

## Evidence log

No runtime evidence collected yet.

## Procedure

1. Reproduce each failure with DCC and GCC.
2. Capture exit status and GDB backtrace/registers.
3. Compare DCC and GCC ELF sections, symbols, relocations, and disassembly.
4. Minimize the confirmed root cause to a test under 30 lines.
5. Modify compiler logic only after the evidence identifies the root cause.
6. Run cargo test and affected torture tests before committing.

## Cleanup

This session remains open until post-fix evidence is compared and the user
confirms the result.
