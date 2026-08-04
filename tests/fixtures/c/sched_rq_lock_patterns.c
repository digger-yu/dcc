// sched_rq_lock_patterns.c - kernel/sched 中的运行队列锁与原子操作模式
//
// 验证 dcc 能编译内核调度器 (kernel/sched/) 中使用的核心同步原语。
// 涵盖 raw_spinlock / seqcount / atomic_t 等调度器关键数据结构。

// raw_spinlock 的最小化定义与操作
typedef struct {
    unsigned int val;
} raw_spinlock_t;

#define __RAW_SPINLOCK_UNLOCKED { .val = 0 }

static inline void raw_spin_lock(raw_spinlock_t *lock) {
    __asm__ volatile(
        "1:\n\t"
        "lock; xchgl %0, %1\n\t"
        "testl %0, %0\n\t"
        "jz 3f\n\t"
        "2:\n\t"
        "pause\n\t"
        "cmpl $0, %1\n\t"
        "jne 2b\n\t"
        "jmp 1b\n\t"
        "3:\n\t"
        : "+r"(*(unsigned int *)lock), "+m"(lock->val)
        :
        : "memory", "cc"
    );
}

static inline void raw_spin_unlock(raw_spinlock_t *lock) {
    __asm__ volatile(
        "movl $0, %0"
        : "+m"(lock->val)
        :
        : "memory"
    );
}

// atomic_t 操作 (sched 中 rq->nr_running 等计数使用)
typedef struct {
    int counter;
} atomic_t;

static inline int atomic_read(const atomic_t *v) {
    return v->counter;
}

static inline void atomic_set(atomic_t *v, int i) {
    v->counter = i;
}

static inline void atomic_add(int i, atomic_t *v) {
    __asm__ volatile(
        "lock; addl %1, %0"
        : "+m"(v->counter)
        : "ir"(i)
        : "memory"
    );
}

static inline int atomic_add_return(int i, atomic_t *v) {
    int result = i;
    __asm__ volatile(
        "lock; xaddl %0, %1"
        : "+r"(result), "+m"(v->counter)
        :
        : "memory", "cc"
    );
    return result + i;
}

// seqcount 读写序列号 (sched 的 load-balance 路径使用)
typedef struct {
    unsigned int sequence;
} seqcount_t;

static inline unsigned int read_seqcount_begin(const seqcount_t *s) {
    unsigned int seq = s->sequence;
    __asm__ volatile("" ::: "memory");
    // 奇数表示写者正在更新, 需要重试
    return seq & 1 ? 0 : seq;
}

static inline int read_seqcount_retry(const seqcount_t *s, unsigned int start) {
    __asm__ volatile("" ::: "memory");
    return s->sequence != start;
}

// sched_clock 时间戳读取 (rdtsc)
static inline unsigned long long sched_clock_read(void) {
    unsigned int lo, hi;
    __asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return ((unsigned long long)hi << 32) | lo;
}

// 测试入口: 验证上述模式均可编译
void sched_rq_lock_test_entry(void) {
    raw_spinlock_t lock = __RAW_SPINLOCK_UNLOCKED;
    atomic_t nr_running = { .counter = 0 };
    seqcount_t seq = { .sequence = 0 };

    raw_spin_lock(&lock);
    atomic_add(1, &nr_running);
    int n = atomic_read(&nr_running);
    (void)n;
    raw_spin_unlock(&lock);

    unsigned int s = read_seqcount_begin(&seq);
    (void)s;
    int retry = read_seqcount_retry(&seq, s);
    (void)retry;

    unsigned long long ts = sched_clock_read();
    (void)ts;
}
