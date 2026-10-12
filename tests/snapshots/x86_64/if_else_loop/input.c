/* Daily hotspot: if-else + loop.
 *
 * Conditional jump shape (je/jne/jg/jle), loop-back jumps, and the
 * comparison that feeds them. Pins the sub/cmp + jcc ordering and
 * the use of %rax vs %eax for the boolean result.
 */
int classify(int x) {
    if (x < 0)       return -1;
    if (x == 0)      return  0;
    return 1;
}

int sum_range(int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        s += i;
    }
    return s;
}
