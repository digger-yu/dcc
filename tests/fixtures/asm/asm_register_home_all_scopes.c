/* 声明「住在寄存器里」这件事，在**每一条**声明通路上都必须成立 —— 包括那些只在发射期才
 * 被登记的子树。本轮实测的四条都是静默失效（同一个变量两个家：读走寄存器、写走栈槽）：
 *
 *  1. `for` 的 init 里声明的绑定变量：`i++` 只写栈槽，循环条件读 %rbx ⇒ **-O0/-O1 无限循环**
 *     （`register_var` 那侧先返过偏移 0，读-改-写那侧又绕开 `home_for`；两处各一半）。
 *  2. 语句表达式 `({...})` 里的声明：那份 `gen_block_item` 的**副本**漏了 `bind_decl_home`
 *     ⇒ `register long j asm("r12")` 整个被忽略（值仍可能对，但家完全没生效，
 *     与同一个声明写在普通块里两个答案）。
 *  3. 同一个副本还漏了 `cleanup_fn` 登记 ⇒ `__attribute__((cleanup))` 写在语句表达式里
 *     从来不会执行（清理事件静默消失）。
 *  4. 带标签的块里的声明：同一条通路，今天只因为没有内存消费者而潜伏。
 *
 * 反向对照 `plain_scope_still_bound` / `narrow_signed_on_register`：修完不得把「绑定生效」
 * 一起改掉 —— 窄有符号类型在寄存器里反复 `++` 仍要按声明宽度截断/扩展。
 */
#include <stdio.h>

static long sink[4];
static int cleanup_hits;

static void hit_cleanup(long *p)
{
    cleanup_hits += (int)(*p);
}

__attribute__((noinline)) static void use(long v)
{
    sink[3] = v;
}

/* 1：for 的 init 里绑 %rbx，`i++` 必须改在 %rbx 上。 */
__attribute__((noinline)) static long for_init_bound(long n)
{
    long sum = 0;
    for (register long i asm("rbx") = 0; i < n; i++) {
        sum += i;
        use(i);
    }
    return sum; /* 0+1+2+3 = 6（能返回就说明循环终止了） */
}

/* 2 + 3：语句表达式里的声明要照常绑上寄存器，cleanup 要照常执行。 */
__attribute__((noinline)) static long stmt_expr_bound(long n)
{
    long v = ({
        register long j asm("r12");
        __attribute__((cleanup(hit_cleanup))) long c = 4;
        j = n + 5;
        j * 2;
    });
    return v + cleanup_hits; /* (4+5)*2 + 4 = 22 */
}

/* 4：带标签的块里的声明，同一条通路。 */
__attribute__((noinline)) static long labeled_block_bound(long n)
{
    long r = 0;
    goto enter;
enter: {
    register long k asm("r14");
    k = n * 3;
    r = k + 1;
}
    return r; /* 4*3+1 = 13 */
}

/* 反向对照：普通作用域里的绑定必须仍然生效。 */
__attribute__((noinline)) static long plain_scope_still_bound(long n)
{
    register long v asm("rbx") = n;
    v = v + 7;
    use(v);
    return v; /* 11 */
}

/* 反向对照：窄有符号类型在寄存器里反复 ++ 仍按声明宽度截断（-56 + 3 = -53）。 */
__attribute__((noinline)) static long narrow_signed_on_register(void)
{
    register signed char c asm("r15") = -56;
    for (int i = 0; i < 3; i++) {
        c += 1;
    }
    c++;
    return (long)c; /* -53 + 1 = -52 */
}

int main(void)
{
    long a = for_init_bound(4);   /* 6 */
    long b = stmt_expr_bound(4);  /* 22 */
    long d = labeled_block_bound(4); /* 13 */
    long e = plain_scope_still_bound(4); /* 11 */
    long f = narrow_signed_on_register(); /* -52 */
    printf("a=%ld b=%ld d=%ld e=%ld f=%ld hits=%d sink3=%ld\n",
           a, b, d, e, f, cleanup_hits, sink[3]);
    return (a == 6 && b == 22 && d == 13 && e == 11 && f == -52 &&
            cleanup_hits == 4 && sink[3] == 11) ? 0 : 1;
}
