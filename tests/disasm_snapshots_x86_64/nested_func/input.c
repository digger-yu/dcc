/* Daily hotspot: nested function (trampoline).
 *
 * Nested functions take a static chain (%r10 as caller-supplied
 * closure pointer). This case pins: the r10 save in the caller,
 * the r10 -> stack slot in the callee prologue, and the closure
 * trampoline on x86_64.
 */
int outer(int x) {
    int acc = 0;
    int inner(int y) {
        return x + y + acc;
    }
    acc = 10;
    return inner(5);
}
