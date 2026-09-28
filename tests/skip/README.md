# tests/skip/

> Skip lists consumed by the external GCC suites (gcc-dg / torture). Per the
> authoritative Tier table in
> [`docs/tests_quality_system_plan.md`](../../docs/tests_quality_system_plan.md)
> **§9**, those two suites are **Tier 1** Linux CI jobs (`gcc-dg.yml`,
> `torture.yml`). This file only navigates to §9 — it does not restate the
> tiering. The skip lists are **not** referenced by `cargo test` directly.

| File | Consumed by |
|------|-------------|
| `gcc_dg_skip.txt`  | `scripts/gcc_dg_test.sh`, `scripts/gcc_dg_summary.sh` |
| `torture_skip.txt` | `scripts/torture_test.sh`, `scripts/torture_compile_test.sh`, `scripts/torture_execute_test.sh`, `scripts/torture_summary.sh` |

## Format

Each line is a glob or test name that the consuming script should skip. The
exact format depends on the script:

* `gcc_dg_skip.txt` — one DG test directory glob per line
  (e.g. `gcc.c-torture/execute/*`).
* `torture_skip.txt` — one torture test name per line
  (e.g. `pr*`).

## When to add an entry

Only when:

* A test is known-failing but the failure is a known compiler limitation
  documented in `docs/roadmap.md`, OR
* A test relies on a missing libc / runtime feature that dcc cannot yet
  provide, OR
* A test is flaky due to environment noise.

Do **not** use skip lists to mask regressions. The Tier 0 acceptance gate
never reads these files.