/* 常量返回的 `static` 桩：调用点必须换成那个常量，从而它下面的死循环真的被消掉。
 *
 * 现场形状（`mm/shmem.c:1917` THP 关闭 + `mm/shmem.c:1958-1968`）：
 *     static unsigned long shmem_suitable_orders(...) { return 0; }
 *     suitable_orders = shmem_suitable_orders(…);
 *     while (suitable_orders) { … 里面有 HPAGE_PMD_* 的 BUILD_BUG() 陷阱 … }
 * 桩返回值不折成常量，循环就活着、陷阱引用就留着 ⇒ 链接失败（实测 `mm/shmem.o` 5 条）。
 *
 * 两层判据同一个夹具：
 *   * 产物级：`.rela.text` 里不得出现任何指向 `trap_in_dead_loop` 的重定位
 *     （循环体真被删掉才会没有；gcc -O2 实测就是没有）；
 *   * 运行级：语义不能被改坏 —— 实参的副作用必须保留、常量非 0 的桩对应的循环
 *     必须照跑、死循环里那个「一定会触发」的陷阱必须从未执行（`trap_hits`/`trap2_hits` 仍为 0）。
 * 其中形状 4 就是 `mm/shmem.c` 的真实形状（常量先存进局部、条件读那个局部），它要靠
 * `propagate_const_locals_in_conditions` 才能消掉死循环。
 */
int trap_hits;
int trap2_hits;
int measure_calls;

/* 陷阱体是**定义**过的：这样产物级判据只剩「有没有重定位」，不受链接能力影响 */
void trap_in_dead_loop(void)
{
        trap_hits++;
}
/* 第二个陷阱：只给「常量先存进局部、循环条件读变量」那一形用（需条件位置代回） */
void trap_in_dead_loop2(void)
{
        trap2_hits++;
}

/* 桩 1：恒返回 0（= THP 关闭时的 shmem_suitable_orders） */
static unsigned long stub_zero(unsigned long seed)
{
        return 0;
}
/* 桩 2：恒返回 4（反向对照：常量非 0 ⇒ 循环体必须活着、必须执行） */
static unsigned long stub_four(unsigned long seed)
{
        return 4;
}
/* 桩 3：返回值依赖形参 ⇒ 不许当常量桩处理 */
static unsigned long stub_dep(unsigned long seed)
{
        return seed;
}

/* 以下三个局部各自踩中条件代回的一条**拒接**条件（§3.23 那版就是缺这些护栏才把 10 条
 * 跑语义的用例打回）：每个都在循环条件里读自己，循环**必须真执行**，执行次数编进返回值 */
static unsigned long addr_taken_loop_count;
static unsigned long mixed_value_loop_count;
static unsigned long shadowed_loop_count;

/* 拒接 a：局部被取过址（指针可能写它）*/
static unsigned long addr_taken_loop(unsigned long n)
{
        unsigned long c = 0;
        unsigned long *p = &c;
        *p = 1;
        while (c) {
                addr_taken_loop_count++;
                c = 0;
        }
        return addr_taken_loop_count;
}

/* 拒接 b：同一个局部被赋过**不同的**常量 */
static unsigned long mixed_value_loop(void)
{
        unsigned long c = 0;
        if (measure_calls >= 0)
                c = 1;
        while (c) {
                mixed_value_loop_count++;
                c = 0;
        }
        return mixed_value_loop_count;
}

/* 拒接 c：同名遮蔽（外层与内层各有一个 `c`）*/
static unsigned long shadowed_loop(void)
{
        unsigned long c = 1;
        {
                unsigned long c = 0;
                while (c) {
                        shadowed_loop_count++;
                        c = 0;
                }
        }
        while (c) {
                shadowed_loop_count++;
                c = 0;
        }
        return shadowed_loop_count;
}

int measure(void)
{
        measure_calls++;
        return 3;
}

int main(void)
{
        int r = 0;

        /* 1) 恒 0 桩：整个循环体（含陷阱）必须是死代码，而**实参的求值必须保留** */
        unsigned long s = stub_zero((unsigned long)measure());
        while (s) {
                if (!(1))
                        trap_in_dead_loop();
                r |= 1;
        }
        if (measure_calls != 1)
                return 11;
        if (trap_hits != 0)
                return 12;

        /* 2) 恒 4 桩：换成常量后循环照跑一次（防止「一律删掉」） */
        unsigned long t = stub_four(0);
        int n = 0;
        while (t) {
                t = 0;
                n++;
        }
        if (n != 1)
                return 13;

        /* 3) 依赖形参的桩：不能被折，实参副作用照常 */
        unsigned long u = stub_dep((unsigned long)measure());
        if (u != 3 || measure_calls != 2)
                return 14;

        /* 4) `mm/shmem.c` 的真实形状：常量**先存进局部**，循环条件读的是那个局部。
         *    需要「写进局部的常量代回条件位置」才删得掉死循环。 */
        unsigned long so = 0;
        so = stub_zero((unsigned long)measure());
        while (so) {
                if (!(1))
                        trap_in_dead_loop2();
                r |= 2;
        }
        if (trap2_hits != 0)
                return 15;
        if (measure_calls != 3)
                return 16;

        /* 5-7) 三条拒接护栏：循环必须**真执行**，否则就是被误折成恒假
         *      a 取址、b 赋了不同常量、c 同名遮蔽，都不得进候选表 */
        if (addr_taken_loop(2) != 1)
                return 17;
        if (mixed_value_loop() != 1)
                return 18;
        /* 内层 c=0 跑 0 次、外层 c=1 跑 1 次，合计 1 */
        if (shadowed_loop() != 1)
                return 19;

        return r;
}
