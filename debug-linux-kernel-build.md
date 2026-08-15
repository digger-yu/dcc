# Debug Session: linux-kernel-build

Status: OPEN

## Goal

Make `make all` drive the Linux 7.1 full build flow for dcc using
`/root/linux-7.1` as the kernel tree.

## Hypotheses

1. The repository has no tracked `Makefile`, so `make all` fails before
   invoking Cargo or kernel scripts.
2. Existing kernel helper scripts still point to an old dcc checkout,
   causing false `dcc not found` failures.
3. The local Rust build or tests may fail before kernel compilation.
4. Linux 7.1 compilation may expose missing C/GNU extension support in
   dcc after the build entry point is fixed.
5. Any compiler diagnostic without an exact file/line will need targeted
   diagnostic improvement after reproducing the failing translation unit.

## Evidence Log

- Initial `make all` failed with no rule for target `all`.
- `/root/linux-7.1` exists and contains a Linux kernel source tree.
