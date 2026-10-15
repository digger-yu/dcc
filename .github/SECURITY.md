# Security Policy

## What dcc is, and is not

dcc is an alpha research compiler. It is not a security boundary, it is not
production-ready, and it has no supported release channels — the only supported
revision is `main`.

Because of that, the honest default is: **most things you would call a security
problem in another project are ordinary bugs here.**

- A compiler crash, hang, or stack overflow on a crafted source file is a bug.
  Open a normal issue.
- A miscompile, including one that silently changes the meaning of a program,
  is a bug. Open a normal issue with a reduced fixture.
- Undefined behavior in the *compiled output* caused by a missing dcc check
  (for example `-fstack-protector` not implemented on a cross back end) is a
  documented gap, not a vulnerability. See `docs/missing_features.md` and the
  Known limitations section of the README.

This mirrors how GCC and Clang treat their own compilers.

## When to report privately instead

Report something privately only if it is about **dcc itself misbehaving as a
program**, in a way that matters to someone running it on input they do not
trust — for example a CI service, a build farm, or a tooling pipeline that
compiles third-party C. That means specifically:

- writing outside the path given to `-o`, or outside the cache directories
  (`~/.dcc/cache`, `.dcc-cache/`), including through a crafted source file name
  or an embedded directive;
- reading files the compiler was never asked to include, or leaking their
  content into its output;
- executing anything that was not requested: dcc does invoke host tools
  (`cc`, `ld`, `objcopy`, `<triple>-as`) when it delegates linking or assembling,
  and a source file that triggers an unintended host command line is in scope;
- a memory-safety failure in the Rust code that is reachable and controllable
  (the release profile builds with `panic = "abort"`, so a plain panic is a
  crash — that is a bug, not a report).

## How to report

1. If private vulnerability reporting is enabled on
   [github.com/digger-yu/dcc](https://github.com/digger-yu/dcc), use the
   **Report a vulnerability** form on the Security tab.
2. Otherwise mail **digger-yu@outlook.com**.

Please do not open a public issue for those cases until it has been triaged.
Include: the dcc commit, the exact command line, the input (reduced if you can),
and what you believe the impact is.

## What to expect — and what not to

- No embargo process, no CVE assignment authority, no coordinated-disclosure
  timeline, and no security advisories guarantee. A single maintainer reads and
  replies when able; responses may be slow, and this document does not promise
  otherwise.
- Fixes land on `main`. There are no backport branches and no supported older
  versions.
- If a report turns out to be "the compiler produced wrong code", it will be
  closed as a normal bug and asked to be reopened as an issue, because that is
  the more useful place for it.

## Out of scope

- Vulnerabilities in the host toolchain dcc delegates to — `cc`, `ld`, `as`,
  `objcopy` — or in the tools its gate scripts call (`objtool`, `pahole`,
  `qemu-system-*`). Report them to those projects.
- Vulnerabilities in the build-time crates (`zstd`, `miniz_oxide`, `anyhow`) —
  report them upstream.
- Security problems in programs compiled by dcc, including the Linux kernel and
  the GCC sources used as test corpora. Those corpora exist to exercise the
  compiler; findings about them are not dcc findings.
- Test fixtures and scratch output under `tests/` and `report/`, which may
  contain deliberately malformed input and local paths.
