/* VRP: 宽字面量（64 位）与 int dest 的比较不得被掩码折错。
 *
 * 根因（2026-10 修）：SCCP 把 `long long` 局部换成无类型 `Int(1099511627776)` 后，
 * VRP 的 `(Int, Int)` 操作数对按 **dest 宽度**掩码——比较的 dest 恒为 `int`（32 位），
 * 于是 1099511627776 被掩成 0，`a <= 0` 折成恒真，`-O2` 下整个 if 被折成 then 分支
 * （`bootstrap_selftest.c` 的 longlong 组因此 rc=-2）。修法：字面量装不下 dest 类型
 * 时本条放弃（少折一条比较不是错值，折错一条是）。
 * 判据：四档全部返回 0；任一条比较被折错都会改变退出码。
 */
int main(void) {
    long long a = 1LL << 40;
    if (a <= 0) return 1;              /* 曾被折成恒真 */
    if (a < 0) return 2;
    if (!(a > 0)) return 3;
    if (0 >= a) return 4;
    long long b = a * 3;
    long long c = b / 3;
    if (c != a) return 5;
    if (a + a != (1LL << 41)) return 6;
    if (a > 0x7fffffffffffffffLL) return 7;
    if (1099511627776LL <= 0) return 8; /* 两个字面量的比较同样走该路径 */
    return 0;
}
