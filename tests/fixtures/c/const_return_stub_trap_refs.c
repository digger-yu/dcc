/* 产物级判据用的夹具：陷阱函数是**只有声明、没有定义**的 extern（内核那一族
 * `__compiletime_assert_NNN` / `__BUILD_BUG_vruntime_cmp` 就是这个形状），所以「调用还在不在」
 * 会真的落到**重定位与 UND 符号**上——链接期就报 `undefined reference`。
 *
 * 为什么另开一个文件而不是复用运行级那个：把陷阱定义在同一个 TU 里，本地 `call` 不需要重定位，
 * 「`.rela.text` 里没有指向它的条目」这条断言就**永远成立**（实测：旧二进制对本文件的形状 4
 * 仍然一条重定位都不产生，而形状 4 的死循环明明还在），判据成了空话。所以这里必须 extern。
 *
 * 四条形状的期望（gcc -O2 实测：全部 0 条）：
 *   1) 恒 0 桩 + 条件里直接用调用  → 靠 fold_const_return_calls 消掉
 *   2) 恒 0 桩 + 常量先存进局部再当条件 → 靠 propagate_const_locals_in_conditions 消掉
 *   3) 恒非 0 的桩的循环             → **必须留着**（反向对照，防「一律删」）
 *   4) 依赖形参的桩                → **必须留着**（拒接对照）
 *   5) 常量在嵌套块里写入、循环条件在外层读（= shmem 真实形状）→ 必须 0 条
 *   6) 桩返回 NULL（指针常量）守住一个无定义的 fs_bio_integrity_* 形调用 → 必须 0 条
 *   7) 桩返回非 NULL → 引用**必须留着**（反向对照）
 */
typedef unsigned long size_t;

extern void trap_direct(void) __attribute__((noreturn));
extern void trap_via_local(void) __attribute__((noreturn));
extern void trap_live_loop(void);
extern void trap_dep_loop(void);
extern void trap_nested_local(void);

static unsigned long stub_zero(unsigned long seed)
{
        return 0;
}
static unsigned long stub_four(unsigned long seed)
{
        return 4;
}
static unsigned long stub_dep(unsigned long seed)
{
        return seed;
}

int g_side;

/* 1) 调用直接坐在循环条件里 */
int s1_direct(unsigned long n)
{
        while (stub_zero(n)) {
                if (!(1))
                        trap_direct();
                g_side = 1;
        }
        return g_side;
}

/* 2) 常量先存进局部，循环条件读那个局部（= `mm/shmem.c` 的真实形状） */
int s2_via_local(unsigned long n)
{
        unsigned long so = 0;
        so = stub_zero(n);
        while (so) {
                if (!(1))
                        trap_via_local();
                g_side = 2;
        }
        return g_side;
}

/* 3) 恒非 0 的桩：循环必须真存在，陷阱调用必须在产物里（反向对照） */
int s3_live_loop(unsigned long n)
{
        int k = 0;
        unsigned long t = stub_four(n);
        while (t) {
                trap_live_loop();
                t = 0;
                k++;
        }
        return k;
}

/* 4) 依赖形参的桩：不许被当常量处理，陷阱调用必须留着（拒接对照） */
int s4_dep_loop(unsigned long n)
{
        int k = 0;
        while (stub_dep(n)) {
                trap_dep_loop();
                n = 0;
                k++;
        }
        return k;
}

/* 5) shmem 的完整形状：赋值发生在**嵌套块**里（`if (orders > 0) { so = stub(); }`），
 * 循环与声明在外层。只考局部只写一次、写入同一常量这一个不变式。 */
int s5_nested_local(unsigned long n)
{
        unsigned long so = 0;
        if (n > 0)
                so = stub_zero(n);
        while (so) {
                if (!(1))
                        trap_nested_local();
                g_side = 5;
        }
        return g_side;
}

/* 6/7) **指针常量**桩：`include/linux/bio-integrity.h:88` 在 CONFIG_BLK_DEV_INTEGRITY
 * 关闭时就是 `static inline struct bio_integrity_payload *bio_integrity(bio) { return NULL; }`，
 * 守着一个全树无定义的 `fs_bio_integrity_*()`。旧实现只认整型返回 ⇒ gcc 不留引用而
 * dcc 留（实测五秒夹具 /tmp/kwred/bip_stub.c）⇒ 全树 18 + 14 行未定义引用。
 * 7) 是反向对照：桩返非 NULL 时引用**必须留着**。 */
struct biod {
        int x;
};
struct bipd {
        int y;
};
extern void fs_guarded_free(struct biod *bio);
extern void fs_guarded_keep(struct biod *bio);

static inline struct bipd *bio_has_integrity(struct biod *bio)
{
        (void)bio;
        return ((void *)0);
}
static inline struct bipd *bio_always_integrity(struct biod *bio)
{
        (void)bio;
        return (struct bipd *)1;
}

void s6_null_guard(struct biod *bio)
{
        if (bio_has_integrity(bio))
                fs_guarded_free(bio);
}
void s7_nonnull_guard(struct biod *bio)
{
        if (bio_always_integrity(bio))
                fs_guarded_keep(bio);
}

/* 8/9) **`static inline` 的整型零桩坐在 `if` 条件里**：`arch/x86/boot/startup/map_kernel.c:49`
 * 的 `if (sme_get_me_mask()) { …rip_rel_ptr(__start_bss_decrypted)… }` 就是这个形状
 * （`arch/x86/include/asm/mem_encrypt.h:98` 在 CONFIG_AMD_MEM_ENCRYPT 关闭时是
 * `static inline u64 sme_get_me_mask(void) { return 0; }`）。它与形状 1 的差别只在
 * **`inline`**：内联先跑时调用点已经不是 `Call` 节点，而是语句块里的 `__inl0$rv` 临时量
 * （`VarDecl` 无初值 + `+= 常量`），条件位置读的是那个局部 —— 桩折叠看不见就一条都消不掉。
 * 实测（快照二进制 + `/tmp/kwred/w.c`）：gcc -O2 留 0 条，而 pass 排在内联之后的实现留 1 条
 * ⇒ 全树 `__pi_` 一族 10 行未定义引用。9) 是反向对照：恒 1 桩守住的调用必须留着。 */
extern void sme_guarded_ref(void);
extern void sme_kept_ref(void);

static inline unsigned long sme_get_me_mask(void)
{
        return 0;
}
static inline int sme_always_on(void)
{
        return 1;
}

unsigned long s8_inline_zero_guard(unsigned long paddr)
{
        if (sme_get_me_mask())
                sme_guarded_ref();
        return paddr + sme_get_me_mask();
}
void s9_inline_one_guard(void)
{
        if (sme_always_on())
                sme_kept_ref();
}

/* 10/11) **死分支里坐一次 `__always_inline` 调用**：内联展开器把 callee 的 `return` 改写成
 * `goto __inlN__end` 时，会在展开块尾部留一条**自造出口标签**。死分支折叠原先的拦网是
 * 「被丢的分支里有 Label 就不删」（防 `goto` 跳进去变成 codegen `unresolved label`），
 * 于是一条自造标签就把整个死分支挡住了。
 * 现场：`arch/x86/boot/startup/gdt_idt.c:67` 的 `handler = rip_rel_ptr(vc_no_ghcb);` 坐在
 * `if (IS_ENABLED(CONFIG_AMD_MEM_ENCRYPT))`（预处理成 `if (0)`）里，而 `rip_rel_ptr()` 是
 * `static __always_inline __pure`（asm.h:119）⇒ `vc_no_ghcb` 留成未定义引用（实测
 * `gdt_idt.o` 2 条重定位）。修法是让这条拦网只认**源码能写出来的**标签。
 * 11) 是反向对照：同样的内联调用坐在真分支里时，调用**必须留着**。 */
extern void lab_guarded_ref(void);
extern void lab_alive_ref(void);

/* 夹具里不能写内核的 `__always_inline` 宏（只有内核头里有），等价写法如下。 */
static inline __attribute__((always_inline)) void *lab_passthru(void *p)
{
        return p;
}

void *g_lab_sink;

void s10_inline_exit_label_in_dead_branch(void)
{
        void *h = (void *)0;

        if (0)
                h = lab_passthru((void *)lab_guarded_ref);
        g_lab_sink = h;
}
void s11_inline_exit_label_in_live_branch(int c)
{
        void *h = (void *)0;

        if (c)
                h = lab_passthru((void *)lab_alive_ref);
        g_lab_sink = h;
}
