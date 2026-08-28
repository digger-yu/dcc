#!/usr/bin/env python3
"""Faithful model of DCC's OLD vs NEW substitute_params, to prove the
TRANSLATE(*d) -> (**d) root cause. Mirrors preproc.rs exactly."""

def skip_strings(text):
    """Scan identifier tokens; replace matching idents (opaque within one pass)."""
    # This mirrors replace_ident / replace_all_params: scan input chars,
    # write to output; written replacement is NOT re-scanned in THIS pass.
    return text  # placeholder, real impl below

def replace_ident(text, frm, to):
    """OLD per-param: replace standalone ident `frm` with `to` (one opaque pass)."""
    out = []
    chars = list(text)
    i, n = 0, len(chars)
    while i < n:
        c = chars[i]
        if c.isalpha() or c == '_':
            j = i
            while j < n and (chars[j].isalnum() or chars[j] == '_'):
                j += 1
            ident = ''.join(chars[i:j])
            out.append(to if ident == frm else ident)
            i = j
        else:
            out.append(c)
            i += 1
    return ''.join(out)

def replace_all_params(text, params, args):
    """NEW: single opaque scan substituting ALL params at once."""
    pmap = {p: a for p, a in zip(params, args)}
    out = []
    chars = list(text)
    i, n = 0, len(chars)
    while i < n:
        c = chars[i]
        if c.isalpha() or c == '_':
            j = i
            while j < n and (chars[j].isalnum() or chars[j] == '_'):
                j += 1
            ident = ''.join(chars[i:j])
            out.append(pmap.get(ident, ident))
            i = j
        else:
            out.append(c)
            i += 1
    return ''.join(out)

def old_handle_paste(body, params, args):
    """OLD: ALWAYS runs replace_ident loop first, even with no ##."""
    result = body
    for i, p in enumerate(params):
        if i < len(args):
            result = replace_ident(result, p, args[i])
    # (## processing omitted - irrelevant for TRANSLATE which has no ##)
    return result

def new_handle_paste(body, params, args):
    """NEW: early-return when no ## -> NO substitution."""
    if '##' not in body:
        return body
    return body  # (##-adjacent-only substitution omitted)

def old_substitute(body, params, args):
    r = old_handle_paste(body, params, args)          # PASS 1
    for i, p in enumerate(params):                     # PASS 2 (third step)
        if i < len(args):
            r = replace_ident(r, p, args[i])
    return r

def new_substitute(body, params, args):
    r = new_handle_paste(body, params, args)           # no-op for TRANSLATE
    r = replace_all_params(r, params, args)            # single opaque pass
    return r

# --- the actual regex.c macro + call ---
BODY  = "(translate ? (char) translate[(unsigned char) (d)] : (char) (d))"
PARAMS = ["d"]
ARGS   = ["*d"]   # the call TRANSLATE(*d)

print("macro body :", BODY)
print("call arg   :", ARGS[0])
print()
print("OLD (pre-fix)  ->", old_substitute(BODY, PARAMS, ARGS))
print("NEW (fixed)    ->", new_substitute(BODY, PARAMS, ARGS))
print()
# also the &di variant (same bug class, ##-bearing macro)
print("=== &di variant (param di, arg &di) ===")
BODY2 = "foo(di ## suffix)"
print("OLD ->", old_substitute(BODY2, ["di"], ["&di"]))
print("NEW ->", new_substitute(BODY2, ["di"], ["&di"]), "(## path keeps param for paste step)")
