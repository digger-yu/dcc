/*
 * 语法：嵌套的两个 `for` 初始化子句各自声明同名的 cleanup 守卫变量（内核
 * `scoped_guard()` 展开后两个守卫都叫 `scope`），且它们作用在**不同对象**上。
 *
 * 现场：7.1 内核 `sched_mm_cid_exit()` / `sched_mm_cid_fork()`
 *
 *   scoped_guard(mutex, &mm->mm_cid.mutex) {          // for (CLASS(mutex, scope)(...))
 *       ...
 *       scoped_guard(raw_spinlock_irq, &mm->mm_cid.lock) {  // 又一个 scope
 *           ...
 *       }
 *   }
 *
 * 块内声明有 rename_shadow_var 兜住同名遮蔽，for-init 里的声明没有 —— 两个
 * `scope` 共用一个帧槽（var_map 以名字索引），内层 spinlock 守卫覆写外层
 * mutex 守卫；外层清理时把 spinlock 指针当 mutex 解，真正的 mm_cid.mutex
 * 永不释放。症状：`pthread_create`/线程退出全部阻塞在
 * sched_mm_cid_fork / sched_mm_cid_exit 的 mutex_lock，4 核全 idle，永久挂死。
 *
 * 判据：每个守卫的析构必须落在**它自己**捕获的对象上。
 * 失败码：1 = 两层嵌套各减各的对象；2 = 三层嵌套；4 = for-init 守卫遮蔽
 * 块内同名普通变量；8 = 同名守卫分别罩不同对象的 count 校验。
 */

struct obj {
    int v;
};

static void drop(struct obj **p) { (*p)->v--; }

/* 1：两层嵌套，外层 a 内层 b —— 各减一次。 */
static int case_two(struct obj *a, struct obj *b)
{
    for (struct obj *scope __attribute__((cleanup(drop))) = a, *done = 0;
         !done; done = (void *)1) {
        a->v += 1;
        for (struct obj *scope __attribute__((cleanup(drop))) = b, *done2 = 0;
             !done2; done2 = (void *)1) {
            b->v += 10;
        }
        a->v += 100;
    }
    /* 正确：a = 1+100-1 = 100，b = 10-1 = 9。 */
    return (a->v == 100 && b->v == 9) ? 0 : 1;
}

/* 2：三层嵌套，三个不同对象。 */
static int case_three(struct obj *a, struct obj *b, struct obj *c)
{
    for (struct obj *scope __attribute__((cleanup(drop))) = a, *d1 = 0;
         !d1; d1 = (void *)1) {
        for (struct obj *scope __attribute__((cleanup(drop))) = b, *d2 = 0;
             !d2; d2 = (void *)1) {
            for (struct obj *scope __attribute__((cleanup(drop))) = c, *d3 = 0;
                 !d3; d3 = (void *)1) {
                c->v += 7;
            }
            b->v += 5;
        }
        a->v += 3;
    }
    return (a->v == 2 && b->v == 4 && c->v == 6) ? 0 : 2;
}

/* 4：for-init 守卫遮蔽块内同名普通变量，块内变量必须保持原值。 */
static int case_shadow_local(void)
{
    int scope = 41;
    struct obj a = {0};

    for (struct obj *scope __attribute__((cleanup(drop))) = &a, *d = 0;
         !d; d = (void *)1) {
        a.v += 1;
    }
    return (scope == 41 && a.v == 0) ? 0 : 4;
}

/* 8：两个守卫捕获同一个对象时，析构次数不能多也不能少。 */
static int case_same_object(struct obj *a)
{
    for (struct obj *scope __attribute__((cleanup(drop))) = a, *d1 = 0;
         !d1; d1 = (void *)1) {
        for (struct obj *scope __attribute__((cleanup(drop))) = a, *d2 = 0;
             !d2; d2 = (void *)1) {
            a->v += 2;
        }
    }
    /* 正确：2 - 1 - 1 = 0。 */
    return (a->v == 0) ? 0 : 8;
}

int main(void)
{
    int bad = 0;
    struct obj a = {0}, b = {0}, c = {0};

    bad |= case_two(&a, &b);

    a.v = b.v = c.v = 0;
    bad |= case_three(&a, &b, &c);

    bad |= case_shadow_local();

    a.v = 0;
    bad |= case_same_object(&a);
    return bad;
}
