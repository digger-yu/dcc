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
 *
 * 输出约定：每个测试项恰好一行，前缀 "RT|"；判据是两份内核日志中
 * 这些行逐字节相同 —— 因此**禁止**打印任何随内核构建而异的内容
 * （版本串、时间戳、pid、中断计数本身、errno 之外的地址等）。
 * 失败行打印 "FAIL:<stage>:<errno>"，同样保持确定性。
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
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
    char line[256];
    long long v = -1;
    FILE *f = fopen("/proc/interrupts", "r");
    if (!f)
        return -1;
    while (fgets(line, sizeof(line), f)) {
        if (strncmp(line, "LOC:", 4) == 0) {
            if (sscanf(line + 4, "%lld", &v) != 1)
                v = -1;
            break;
        }
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
    if (umount2("/mnt", 0) != 0)
        return rt("ext4|FAIL:umount"), 1;
    rt("ext4|magic roundtrip umount");
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
    fail |= test_ext4();
    dprintf(1, "RT|exit|%d\n", fail);
    return fail;
}
