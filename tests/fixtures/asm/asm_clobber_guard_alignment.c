/* 守护是「进 asm 前 push、出 asm 后 pop」，于是 %rsp 在 asm 体内被下推 8×N 字节。
 * 本后端有一条写死的不变式：**体内每个 call 点必须 rsp ≡ 0 (mod 16)** ——
 * `emit/stmt.rs` 里 pr44575 的教训就是「switch 的 case 体跨调用多压 8 字节」让 libc 的
 * `movaps` 当场对齐崩溃（那个缺陷的修法是改用帧槽，而不是补对齐）。P12 Phase 5.2a 的
 * clobber 守护也撞上同一件事：奇数个守护 push 必须配一次 `sub/add rsp, 8`，偶数个不配。
 *
 * 为什么这份夹具**不**与 gcc 对照 stdout：在 inline asm 里 `call` 一个函数是 GCC 文档明确
 * 不支持的写法（编译器不为这个调用维护栈帧），实测 gcc 连活变量 `b` 都保不住（返回一个栈
 * 地址），而 dcc 保住了。于是要钉的性质是本后端自己的 ABI 不变式 + dcc 侧的确定性读数，
 * 参照系不可能是 gcc。产物级的判据在 tests/integration/c_asm.rs（逐 call 点算 %rsp 位移）。
 */
long printf(const char *, ...);

__attribute__((noinline)) long leaf(long z)
{
    return z * 2 + 1;
}

/* 奇数个守护：-O2 下 `a` 的家是 %rbx，而这条 asm 宣告毁掉它 ⇒ 守护 1 个号 ⇒ 需要补对齐。 */
__attribute__((noinline)) long one_guarded(long n)
{
    long a = 0, b = 0;
    for (long i = 0; i < n; i++) {
        a += 1;
        b += a;
        __asm__ volatile("call leaf"
                         :
                         : "D"(a)
                         : "rax", "rcx", "rdx", "rsi", "r8", "r9", "r10", "r11", "rbx", "memory");
    }
    return b; /* n=4 ⇒ 0+1+2+3+4 = 10 */
}

/* 偶数个守护：%rbx 与 %r12 同时是某些变量的家且都被这条 asm 毁掉 ⇒ 2 个 push 已经对齐，
 * 不得再多塞一次 `sub rsp, 8`（那会把对齐又弄坏，是这条判据的反向对照）。 */
__attribute__((noinline)) long two_guarded(long n)
{
    long a = 0, b = 0, c = 0, d = 0, e = 0;
    for (long i = 0; i < n; i++) {
        a += 1;
        b += 2;
        c += 3;
        d += 4;
        __asm__ volatile("call leaf"
                         :
                         : "D"(a)
                         : "rax", "rcx", "rdx", "rsi", "r8", "r9", "r10", "r11", "rbx", "r12",
                           "memory");
        e += a + b + c + d;
    }
    return a + b + c + d + e; /* n=4 ⇒ 4+8+12+16+100 = 140 */
}

int main(void)
{
    long r1 = one_guarded(4);
    long r2 = two_guarded(4);
    printf("r1=%ld r2=%ld\n", r1, r2);
    return (r1 == 10 && r2 == 140) ? 0 : 1;
}
