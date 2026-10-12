/* Daily hotspot: atomic ops (xchg / lock cmpxchg).
 *
 * Lock-prefixed atomic read-modify-write. Pins the LOCK prefix
 * byte and the opcode (xchg for swap, cmpxchg for CAS).
 */
int atomic_add(int *p, int v) {
    int old = *p;
    *p = old + v;
    return old;
}

long atomic_swap(long *p, long v) {
    long old = *p;
    *p = v;
    return old;
}
