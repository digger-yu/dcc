/* 运行时闭环测试程序（initramfs 内由 /init 脚本调起）。
 *
 * 目的：在 dcc 编译的内核与 gcc 编译的内核上各跑一遍，验证
 *   - 基本系统调用（getpid/getuid/uname/open/write/read/stat/close）
 *   - fork/exec/wait4（子进程 exec /t2，退出码必须原样传回）
 *   - pipe + fork（跨进程字节流）
 *   - 定时器（setitimer ITIMER_REAL + SIGALRM + sigsuspend）
 *   - 时钟（clock_gettime CLOCK_MONOTONIC 单调且 nanosleep 到点）
 *   - 中断（/proc/interrupts 的 LOC 计数在两次采样之间增长）
 *   - 文件系统（initramfs 根可写；/dev/vda 上的 ext4 可挂载）
 *   - 多核（-smp 下 online CPU 数、/proc/cpuinfo 与 /proc/interrupts
 *     每 CPU 列数三者必须一致）
 *   - 多人同跑（8 进程 + 4 线程对共享内存做原子累加，总数必须精确）
 *   - 块设备 I/O（ext4 上 256 KiB 分块写 + fsync + 回读，4 进程并发写）
 *   - 网络（virtio-net 配 IP 后发 UDP 触发 ARP，网关 ARP 表项必须 complete）
 *
 * 输出约定：每个测试项恰好一行，前缀 "RT|"；判据是两份内核日志中
 * 这些行逐字节相同 —— 因此**禁止**打印任何随内核构建而异的内容
 * （版本串、时间戳、pid、中断计数本身、errno 之外的地址等）。
 * 失败行打印 "FAIL:<stage>:<errno>"，同样保持确定性。
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <netinet/in.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/statfs.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void rt(const char *line) {
    dprintf(1, "RT|%s\n", line);
}

/* ---- 1. 基本系统调用 ------------------------------------------------ */
static int test_basic(void) {
    struct utsname u;
    if (getpid() <= 0)
        return rt("basic|FAIL:getpid"), 1;
    if (getuid() != 0 || geteuid() != 0)
        return rt("basic|FAIL:uid"), 1;
    if (uname(&u) != 0)
        return rt("basic|FAIL:uname"), 1;
    /* 只校验不随构建变化的两个字段（release/version 会变，不打印）。 */
    if (strcmp(u.sysname, "Linux") != 0 || strcmp(u.machine, "x86_64") != 0)
        return rt("basic|FAIL:name"), 1;
    rt("basic|pid uid uname");
    return 0;
}

/* ---- 2. initramfs 上的文件往返 -------------------------------------- */
static int test_file(void) {
    const char *msg = "dcc-runtime-probe\n";
    char buf[64];
    struct stat st;
    int fd = open("/rt-file", O_CREAT | O_TRUNC | O_RDWR, 0644);
    if (fd < 0)
        return rt("file|FAIL:open"), 1;
    if (write(fd, msg, strlen(msg)) != (ssize_t)strlen(msg))
        return rt("file|FAIL:write"), 1;
    if (lseek(fd, 0, SEEK_SET) != 0)
        return rt("file|FAIL:lseek"), 1;
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, sizeof(buf) - 1) != (ssize_t)strlen(msg))
        return rt("file|FAIL:read"), 1;
    if (strcmp(buf, msg) != 0)
        return rt("file|FAIL:content"), 1;
    if (fstat(fd, &st) != 0 || st.st_size != (off_t)strlen(msg))
        return rt("file|FAIL:stat"), 1;
    close(fd);
    if (unlink("/rt-file") != 0)
        return rt("file|FAIL:unlink"), 1;
    rt("file|write read stat unlink");
    return 0;
}

/* ---- 3. fork + wait4 ------------------------------------------------ */
static int test_fork(void) {
    int st = 0;
    pid_t p = fork();
    if (p < 0)
        return rt("fork|FAIL:fork"), 1;
    if (p == 0)
        _exit(7);
    if (wait4(p, &st, 0, NULL) != p)
        return rt("fork|FAIL:wait4"), 1;
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 7)
        return rt("fork|FAIL:status"), 1;
    rt("fork|child-status=7");
    return 0;
}

/* ---- 4. fork + execve + wait4（exec 后的子进程自己打印一行） --------- */
static int test_exec(void) {
    int st = 0;
    pid_t p = fork();
    if (p < 0)
        return rt("exec|FAIL:fork"), 1;
    if (p == 0) {
        execl("/t2", "t2", (char *)NULL);
        _exit(111);
    }
    if (wait4(p, &st, 0, NULL) != p)
        return rt("exec|FAIL:wait4"), 1;
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 7)
        return rt("exec|FAIL:status"), 1;
    rt("exec|child-status=7");
    return 0;
}

/* ---- 5. pipe + fork ------------------------------------------------- */
static int test_pipe(void) {
    int fds[2];
    char buf[8];
    int st = 0;
    if (pipe(fds) != 0)
        return rt("pipe|FAIL:pipe"), 1;
    pid_t p = fork();
    if (p < 0)
        return rt("pipe|FAIL:fork"), 1;
    if (p == 0) {
        close(fds[0]);
        if (write(fds[1], "ping", 4) != 4)
            _exit(1);
        _exit(0);
    }
    close(fds[1]);
    memset(buf, 0, sizeof(buf));
    if (read(fds[0], buf, sizeof(buf) - 1) != 4)
        return rt("pipe|FAIL:read"), 1;
    close(fds[0]);
    if (strcmp(buf, "ping") != 0)
        return rt("pipe|FAIL:content"), 1;
    if (wait4(p, &st, 0, NULL) != p || !WIFEXITED(st) || WEXITSTATUS(st) != 0)
        return rt("pipe|FAIL:child"), 1;
    rt("pipe|ping");
    return 0;
}

/* ---- 6. setitimer + SIGALRM ----------------------------------------- */
static volatile sig_atomic_t alarm_count;

static void on_alarm(int sig) {
    (void)sig;
    alarm_count++;
}

static int test_timer(void) {
    struct sigaction sa;
    struct itimerval it;
    sigset_t block, old;
    int i;

    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_alarm;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGALRM, &sa, NULL) != 0)
        return rt("timer|FAIL:sigaction"), 1;

    /* 先屏蔽 SIGALRM，才能在 sigsuspend(&old) 里原子地等待。 */
    sigemptyset(&block);
    sigaddset(&block, SIGALRM);
    if (sigprocmask(SIG_BLOCK, &block, &old) != 0)
        return rt("timer|FAIL:sigmask"), 1;

    memset(&it, 0, sizeof(it));
    it.it_value.tv_usec = 20000;
    it.it_interval.tv_usec = 20000;
    if (setitimer(ITIMER_REAL, &it, NULL) != 0)
        return rt("timer|FAIL:setitimer"), 1;

    for (i = 0; i < 200 && alarm_count < 3; i++)
        sigsuspend(&old);

    memset(&it, 0, sizeof(it));
    setitimer(ITIMER_REAL, &it, NULL); /* 停表 */
    sigprocmask(SIG_SETMASK, &old, NULL);

    if (alarm_count < 3)
        return rt("timer|FAIL:count"), 1;
    rt("timer|alarms>=3");
    return 0;
}

/* ---- 7. CLOCK_MONOTONIC + nanosleep --------------------------------- */
static int test_clock(void) {
    struct timespec a, b;
    long long delta;
    if (clock_gettime(CLOCK_MONOTONIC, &a) != 0)
        return rt("clock|FAIL:gettime"), 1;
    usleep(60000);
    if (clock_gettime(CLOCK_MONOTONIC, &b) != 0)
        return rt("clock|FAIL:gettime2"), 1;
    delta = (b.tv_sec - a.tv_sec) * 1000000000LL + (b.tv_nsec - a.tv_nsec);
    if (delta < 50000000LL)
        return rt("clock|FAIL:short"), 1;
    rt("clock|monotonic");
    return 0;
}

/* ---- 8. /proc/interrupts 的 LOC 计数 --------------------------------- */
static long long read_loc(void) {
    char line[512];
    long long v = -1;
    FILE *f = fopen("/proc/interrupts", "r");
    if (!f)
        return -1;
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "LOC:", 4) != 0)
            continue;
        /* 逐 CPU 求和：空闲 CPU 的 tick 会被 NOHZ 停掉，只看一列会假失败。 */
        v = 0;
        char *p = line + 4;
        while (*p) {
            while (*p == ' ' || *p == '\t')
                p++;
            if (*p < '0' || *p > '9')
                break;
            v += strtoll(p, &p, 10);
        }
        break;
    }
    fclose(f);
    return v;
}

static int test_irq(void) {
    long long c1 = read_loc();
    if (c1 < 0)
        return rt("irq|FAIL:read"), 1;
    usleep(200000);
    long long c2 = read_loc();
    if (c2 < 0)
        return rt("irq|FAIL:read2"), 1;
    /* 只判增长，不打印计数值本身（它随构建/调度必然不同）。 */
    if (c2 <= c1)
        return rt("irq|FAIL:stuck"), 1;
    rt("irq|loc-advancing");
    return 0;
}

/* ---- 10. 多核：online / cpuinfo / 每 CPU 列数三者一致 ----------------- */
static int count_online_cpus(void) {
    char buf[128] = {0};
    FILE *f = fopen("/sys/devices/system/cpu/online", "r");
    if (!f)
        return -1;
    if (!fgets(buf, sizeof(buf), f)) {
        fclose(f);
        return -1;
    }
    fclose(f);
    int total = 0;
    char *p = buf;
    while (*p && *p != '\n') {
        while (*p == ',' || *p == ' ')
            p++;
        if (!*p || *p == '\n')
            break;
        char *end;
        long a = strtol(p, &end, 10);
        if (end == p)
            return -1;
        long b = a;
        p = end;
        if (*p == '-') {
            b = strtol(p + 1, &end, 10);
            if (end == p + 1)
                return -1;
            p = end;
        }
        total += (int)(b - a + 1);
    }
    return total;
}

static int count_cpuinfo(void) {
    char line[256];
    int n = 0;
    FILE *f = fopen("/proc/cpuinfo", "r");
    if (!f)
        return -1;
    while (fgets(line, sizeof(line), f))
        if (strncmp(line, "processor", 9) == 0)
            n++;
    fclose(f);
    return n;
}

/* /proc/interrupts 的 LOC 行每 CPU 一列，列数应等于 online CPU 数。 */
static int loc_columns(void) {
    char line[512];
    int n = 0;
    FILE *f = fopen("/proc/interrupts", "r");
    if (!f)
        return -1;
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "LOC:", 4) != 0)
            continue;
        char *p = line + 4;
        while (*p) {
            while (*p == ' ' || *p == '\t')
                p++;
            if (*p < '0' || *p > '9')
                break;
            while (*p >= '0' && *p <= '9')
                p++;
            n++;
        }
        fclose(f);
        return n;
    }
    fclose(f);
    return -1;
}

static int test_smp(void) {
    int on = count_online_cpus();
    if (on <= 0)
        return rt("smp|FAIL:online"), 1;
    if (count_cpuinfo() != on)
        return rt("smp|FAIL:cpuinfo"), 1;
    if (loc_columns() != on)
        return rt("smp|FAIL:loc-cols"), 1;
    char line[64];
    snprintf(line, sizeof(line), "smp|online=%d loc-cols=%d", on, on);
    rt(line);
    return 0;
}

/* ---- 11. 8 进程并发原子累加（共享内存 barrier + 精确总数） ---------- */
struct sched_shm {
    int ready;
    int go;
    long counter;
};

static int test_sched_parallel(void) {
    enum { N = 8, ITERS = 20000 };
    struct sched_shm *sh = mmap(NULL, sizeof(*sh), PROT_READ | PROT_WRITE,
                                MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (sh == MAP_FAILED)
        return rt("sched|FAIL:mmap"), 1;
    memset(sh, 0, sizeof(*sh));
    pid_t pids[N];
    for (int i = 0; i < N; i++) {
        pid_t p = fork();
        if (p < 0)
            return rt("sched|FAIL:fork"), 1;
        if (p == 0) {
            __atomic_fetch_add(&sh->ready, 1, __ATOMIC_SEQ_CST);
            while (!__atomic_load_n(&sh->go, __ATOMIC_ACQUIRE))
                ;
            for (int k = 0; k < ITERS; k++)
                __atomic_fetch_add(&sh->counter, 1, __ATOMIC_SEQ_CST);
            _exit(i + 1);
        }
        pids[i] = p;
    }
    /* 等到 8 个子进程全部就位再放行：证明它们确实同时处于可运行态。 */
    for (int spin = 0; spin < 2000000; spin++) {
        if (__atomic_load_n(&sh->ready, __ATOMIC_ACQUIRE) == N)
            break;
    }
    if (__atomic_load_n(&sh->ready, __ATOMIC_ACQUIRE) != N)
        return rt("sched|FAIL:ready"), 1;
    __atomic_store_n(&sh->go, 1, __ATOMIC_RELEASE);
    int bad = 0;
    for (int i = 0; i < N; i++) {
        int st = 0;
        if (wait4(pids[i], &st, 0, NULL) != pids[i] || !WIFEXITED(st) ||
            WEXITSTATUS(st) != i + 1)
            bad = 1;
    }
    if (bad)
        return rt("sched|FAIL:child-status"), 1;
    if (sh->counter != (long)N * ITERS)
        return rt("sched|FAIL:counter"), 1;
    char line[64];
    snprintf(line, sizeof(line), "sched|%dx%d=%ld", N, ITERS, sh->counter);
    rt(line);
    munmap(sh, sizeof(*sh));
    return 0;
}

/* ---- 12. 4 线程并发原子累加（clone + TLS + 调度） -------------------- */
struct thr_shm {
    long counter;
};

static void *thr_worker(void *arg) {
    struct thr_shm *t = arg;
    for (int k = 0; k < 20000; k++)
        __atomic_fetch_add(&t->counter, 1, __ATOMIC_SEQ_CST);
    return NULL;
}

static int test_threads(void) {
    enum { N = 4, ITERS = 20000 };
    struct thr_shm *t = mmap(NULL, sizeof(*t), PROT_READ | PROT_WRITE,
                             MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (t == MAP_FAILED)
        return rt("threads|FAIL:mmap"), 1;
    t->counter = 0;
    pthread_t th[N];
    for (int i = 0; i < N; i++) {
        if (pthread_create(&th[i], NULL, thr_worker, t) != 0)
            return rt("threads|FAIL:create"), 1;
    }
    for (int i = 0; i < N; i++)
        pthread_join(th[i], NULL);
    if (t->counter != (long)N * ITERS)
        return rt("threads|FAIL:counter"), 1;
    char line[64];
    snprintf(line, sizeof(line), "threads|%dx%d=%ld", N, ITERS, t->counter);
    rt(line);
    munmap(t, sizeof(*t));
    return 0;
}

/* ---- 13. 块设备 I/O：ext4 上分块写 + fsync + 回读，4 进程并发写 ------ */
static void blk_pattern(unsigned char *b, size_t n, int seed) {
    for (size_t i = 0; i < n; i++)
        b[i] = (unsigned char)((seed * 31 + (int)i * 7) & 0xff);
}

static int test_blk_stress(void) {
    enum { CHUNK = 4096, CHUNKS = 64, WRITERS = 4, WCHUNKS = 16 };
    unsigned char *buf = malloc(CHUNK);
    unsigned char *rd = malloc(CHUNK);
    if (!buf || !rd) {
        free(buf);
        free(rd);
        return rt("blk|FAIL:malloc"), 1;
    }

    int fd = open("/mnt/blk-stress", O_CREAT | O_TRUNC | O_RDWR, 0644);
    if (fd < 0) {
        free(buf);
        free(rd);
        return rt("blk|FAIL:open"), 1;
    }
    for (int c = 0; c < CHUNKS; c++) {
        blk_pattern(buf, CHUNK, c);
        if (pwrite(fd, buf, CHUNK, (off_t)c * CHUNK) != CHUNK) {
            close(fd);
            free(buf);
            free(rd);
            return rt("blk|FAIL:write"), 1;
        }
    }
    if (fsync(fd) != 0) {
        close(fd);
        free(buf);
        free(rd);
        return rt("blk|FAIL:fsync"), 1;
    }
    for (int c = 0; c < CHUNKS; c++) {
        blk_pattern(buf, CHUNK, c);
        if (pread(fd, rd, CHUNK, (off_t)c * CHUNK) != CHUNK) {
            close(fd);
            free(buf);
            free(rd);
            return rt("blk|FAIL:read"), 1;
        }
        if (memcmp(buf, rd, CHUNK) != 0) {
            close(fd);
            free(buf);
            free(rd);
            return rt("blk|FAIL:content"), 1;
        }
    }
    close(fd);

    /* 4 个子进程各写 64 KiB（各自独立文件），再逐个回读校验。 */
    pid_t pids[WRITERS];
    for (int w = 0; w < WRITERS; w++) {
        pid_t p = fork();
        if (p < 0) {
            free(buf);
            free(rd);
            return rt("blk|FAIL:fork"), 1;
        }
        if (p == 0) {
            char path[32];
            snprintf(path, sizeof(path), "/mnt/blk-w%d", w);
            int f = open(path, O_CREAT | O_TRUNC | O_RDWR, 0644);
            if (f < 0)
                _exit(1);
            for (int c = 0; c < WCHUNKS; c++) {
                blk_pattern(buf, CHUNK, 100 + w * 17 + c);
                if (pwrite(f, buf, CHUNK, (off_t)c * CHUNK) != CHUNK)
                    _exit(2);
            }
            if (fsync(f) != 0)
                _exit(3);
            close(f);
            _exit(0);
        }
        pids[w] = p;
    }
    int bad = 0;
    for (int w = 0; w < WRITERS; w++) {
        int st = 0;
        if (wait4(pids[w], &st, 0, NULL) != pids[w] || !WIFEXITED(st) ||
            WEXITSTATUS(st) != 0)
            bad = 1;
    }
    for (int w = 0; w < WRITERS && bad == 0; w++) {
        char path[32];
        snprintf(path, sizeof(path), "/mnt/blk-w%d", w);
        int f = open(path, O_RDONLY);
        if (f < 0) {
            bad = 1;
            break;
        }
        for (int c = 0; c < WCHUNKS; c++) {
            blk_pattern(rd, CHUNK, 100 + w * 17 + c);
            if (pread(f, buf, CHUNK, (off_t)c * CHUNK) != CHUNK) {
                bad = 1;
                break;
            }
            if (memcmp(buf, rd, CHUNK) != 0) {
                bad = 1;
                break;
            }
        }
        close(f);
        unlink(path);
    }
    unlink("/mnt/blk-stress");
    free(buf);
    free(rd);
    if (bad)
        return rt("blk|FAIL:parallel"), 1;
    rt("blk|256KiB fsync 4x64KiB");
    return 0;
}

/* ---- 9. ext4（/dev/vda）挂载 + 往返 ---------------------------------- */
static int test_ext4(void) {
    const char *msg = "dcc-ext4-probe\n";
    char buf[64];
    struct statfs sf;

    if (access("/dev/vda", F_OK) != 0)
        return rt("ext4|SKIP:no-device"), 0;
    if (mount("/dev/vda", "/mnt", "ext4", 0, NULL) != 0)
        return rt("ext4|FAIL:mount"), 1;
    if (statfs("/mnt", &sf) != 0) {
        umount2("/mnt", 0);
        return rt("ext4|FAIL:statfs"), 1;
    }
    if ((unsigned long)sf.f_type != 0xEF53UL) { /* EXT4_SUPER_MAGIC */
        umount2("/mnt", 0);
        return rt("ext4|FAIL:magic"), 1;
    }
    int fd = open("/mnt/probe", O_CREAT | O_TRUNC | O_RDWR, 0644);
    if (fd < 0) {
        umount2("/mnt", 0);
        return rt("ext4|FAIL:open"), 1;
    }
    if (write(fd, msg, strlen(msg)) != (ssize_t)strlen(msg)) {
        close(fd);
        umount2("/mnt", 0);
        return rt("ext4|FAIL:write"), 1;
    }
    lseek(fd, 0, SEEK_SET);
    memset(buf, 0, sizeof(buf));
    if (read(fd, buf, sizeof(buf) - 1) != (ssize_t)strlen(msg)) {
        close(fd);
        umount2("/mnt", 0);
        return rt("ext4|FAIL:read"), 1;
    }
    close(fd);
    if (strcmp(buf, msg) != 0) {
        umount2("/mnt", 0);
        return rt("ext4|FAIL:content"), 1;
    }
    unlink("/mnt/probe");
    int blk_fail = test_blk_stress();
    if (umount2("/mnt", 0) != 0)
        return rt("ext4|FAIL:umount"), 1;
    rt("ext4|magic roundtrip umount");
    return blk_fail;
}

/* ---- 14. virtio-net：配 IP + UP，发 UDP 触发 ARP --------------------- */
static int net_first_iface(char *out, size_t n) {
    DIR *d = opendir("/sys/class/net");
    if (!d)
        return -1;
    struct dirent *e;
    int found = -1;
    while ((e = readdir(d)))
        if (e->d_name[0] != '.' && strcmp(e->d_name, "lo") != 0 && strlen(e->d_name) < n) {
            memcpy(out, e->d_name, strlen(e->d_name) + 1);
            found = 0;
            break;
        }
    closedir(d);
    return found;
}

static int iface_up(const char *name) {
    struct ifreq ifr;
    int s = socket(AF_INET, SOCK_DGRAM, 0);
    if (s < 0)
        return -1;
    memset(&ifr, 0, sizeof(ifr));
    strncpy(ifr.ifr_name, name, IFNAMSIZ - 1);
    ifr.ifr_name[IFNAMSIZ - 1] = '\0';
    struct sockaddr_in *sin = (struct sockaddr_in *)&ifr.ifr_addr;
    sin->sin_family = AF_INET;
    sin->sin_addr.s_addr = htonl(0x0a00020f); /* 10.0.2.15，slirp 约定 */
    if (ioctl(s, SIOCSIFADDR, &ifr) != 0) {
        close(s);
        return -1;
    }
    sin->sin_addr.s_addr = htonl(0xffffff00);
    if (ioctl(s, SIOCSIFNETMASK, &ifr) != 0) {
        close(s);
        return -1;
    }
    if (ioctl(s, SIOCGIFFLAGS, &ifr) != 0) {
        close(s);
        return -1;
    }
    ifr.ifr_flags |= IFF_UP;
    if (ioctl(s, SIOCSIFFLAGS, &ifr) != 0) {
        close(s);
        return -1;
    }
    close(s);
    return 0;
}

/* 失败现场：ARP 表原文 + 网卡计数。前缀 RTD| 不进两内核对比（MAC/计数会随构建
 * 与 QEMU 变化），只在 FAIL 路径上打印，用来区分「没发出去」「发了没回」
 * 「回了没解析」三种情况。
 */
static void net_diag(const char *ifname, int serr) {
    static const char *const stats[] = {
        "tx_packets", "rx_packets", "tx_bytes", "rx_bytes",
        "tx_errors", "rx_errors", "tx_dropped", "rx_dropped",
    };
    char line[512], path[160];
    FILE *f;

    dprintf(1, "RTD|sendto-errno|%d\n", serr);
    f = fopen("/proc/net/route", "r");
    if (!f) {
        dprintf(1, "RTD|route|open-failed\n");
    } else {
        int n = 0;
        while (fgets(line, sizeof(line), f) && n++ < 8) {
            line[strcspn(line, "\n")] = '\0';
            dprintf(1, "RTD|route|%s\n", line);
        }
        fclose(f);
    }
    f = fopen("/proc/net/arp", "r");
    if (!f) {
        dprintf(1, "RTD|arp-table|open-failed\n");
    } else {
        int n = 0;
        while (fgets(line, sizeof(line), f) && n++ < 8) {
            line[strcspn(line, "\n")] = '\0';
            dprintf(1, "RTD|arp-table|%s\n", line);
        }
        fclose(f);
    }
    for (unsigned i = 0; i < sizeof(stats) / sizeof(stats[0]); i++) {
        snprintf(path, sizeof(path), "/sys/class/net/%s/statistics/%s",
                 ifname, stats[i]);
        f = fopen(path, "r");
        if (!f) {
            dprintf(1, "RTD|%s|open-failed\n", stats[i]);
            continue;
        }
        if (fgets(line, sizeof(line), f)) {
            line[strcspn(line, "\n")] = '\0';
            dprintf(1, "RTD|%s|%s\n", stats[i], line);
        }
        fclose(f);
    }
}

/* 返回 /proc/net/arp 中该 IP 的 flags，未找到为 -1；0x2 = ATF_COMplete。 */
static int read_arp_flags(const char *ip) {
    char line[512];
    FILE *f = fopen("/proc/net/arp", "r");
    if (!f)
        return -1;
    int flags = -1;
    while (fgets(line, sizeof(line), f)) {
        char a[64], hw[64], fl[64], mask[64], dev[64];
        if (sscanf(line, "%63s %63s %63s %63s %63s", a, hw, fl, mask, dev) != 5)
            continue;
        if (strcmp(a, ip) != 0)
            continue;
        flags = (int)strtol(fl, NULL, 0);
        break;
    }
    fclose(f);
    return flags;
}

static int test_net(void) {
    char ifname[IFNAMSIZ] = {0};
    if (net_first_iface(ifname, sizeof(ifname)) != 0)
        return rt("net|SKIP:no-nic"), 0;
    if (iface_up(ifname) != 0)
        return rt("net|FAIL:ifup"), 1;

    int u = socket(AF_INET, SOCK_DGRAM, 0);
    if (u < 0)
        return rt("net|FAIL:socket"), 1;
    struct sockaddr_in dst;
    memset(&dst, 0, sizeof(dst));
    dst.sin_family = AF_INET;
    dst.sin_port = htons(9);
    inet_pton(AF_INET, "10.0.2.2", &dst.sin_addr); /* QEMU slirp 网关 */
    char b = 'x';
    int flags = -1;
    int serr = 0;
    for (int i = 0; i < 100; i++) {
        errno = 0;
        if (sendto(u, &b, 1, 0, (struct sockaddr *)&dst, sizeof(dst)) < 0)
            serr = errno;
        flags = read_arp_flags("10.0.2.2");
        if (flags >= 0 && (flags & 0x2))
            break;
        usleep(20000);
    }
    close(u);
    if (flags < 0 || !(flags & 0x2)) {
        net_diag(ifname, serr);
        return rt("net|FAIL:arp"), 1;
    }
    char line[64];
    snprintf(line, sizeof(line), "net|%s arp-complete", ifname);
    rt(line);
    return 0;
}

int main(void) {
    int fail = 0;
    fail |= test_basic();
    fail |= test_file();
    fail |= test_fork();
    fail |= test_exec();
    fail |= test_pipe();
    fail |= test_timer();
    fail |= test_clock();
    fail |= test_irq();
    fail |= test_smp();
    fail |= test_sched_parallel();
    fail |= test_threads();
    fail |= test_ext4();
    fail |= test_net();
    dprintf(1, "RT|exit|%d\n", fail);
    return fail;
}
