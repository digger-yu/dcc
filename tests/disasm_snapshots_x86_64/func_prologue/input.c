/* Daily hotspot: function prologue/epilogue.
 *
 * A non-trivial frame with several spilled locals exercises the
 * push %r13 / mov %rsp,%r13 / sub $N,%rsp sequence and the matching
 * epilogue (mov %r13,%rsp; pop %r13; ret). Frame size is the load-
 * bearing bit - the P12-SA regalloc pass recently changed how the
 * 0x3f0 shadow area is reserved and this snapshot pins the layout.
 */
long frame_probe(long a, long b, long c, long d, long e, long f) {
    long buf[8];
    buf[0] = a; buf[1] = b; buf[2] = c; buf[3] = d;
    buf[4] = e; buf[5] = f; buf[6] = a + b; buf[7] = c + d;
    return buf[0] + buf[6] - buf[3];
}
