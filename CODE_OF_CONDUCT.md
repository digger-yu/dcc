# Code of Conduct

## Our expectation

dcc is a solo research project with an open repository. Participation here — issues, pull
requests, code review comments and commit messages — is expected to be about the code.

- Be direct about technical problems and vague about nothing. A concrete "this miscompiles
  `for (int i = 0; i < n; i++)` on aarch64 with `-O2`, here is the reduced fixture" is worth
  more than any amount of praise or hedging.
- Disagree with the change, not the person. Review of a patch is not review of a programmer.
- Do not claim a result you cannot show. This applies to bug reports too: an unreproducible
  "it segfaults sometimes" is not actionable, while `gcc.c-torture` output numbers are.
- Assume good faith on both sides. Maintainers may close issues without a timely reply; that is
  a capacity limit, not a judgement of the reporter.

## Scope

This applies to the repository and to any space where someone is representing the project —
GitHub issues and pull requests, patch review, and mentions of dcc elsewhere.

## Reporting

Report unacceptable behavior to the maintainer at **digger-yu@outlook.com**. Include the
context (issue, pull request, commit) and what happened. This project has no conduct committee;
the maintainer reads reports personally, and the response may be a warning, an edit request, a
block from the repository, or simply no reply if the reporter asks for nothing further. Given
that it is one person, no timeline or appeal structure is promised — that limitation is stated
rather than papered over.

## Attribution

This document is adapted from the [Contributor Covenant](https://www.contributor-covenant.org/),
version 2.1, but deliberately shortened: it names the behavior expected in a compiler
repository instead of importing the full enforcement and response-tier text.
