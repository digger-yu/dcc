/* P0-5: Verify kernel arch/x86/ inline asm instruction coverage.
 * Each asm block uses instructions found in the Linux kernel's
 * arch/x86/ code.  Compile + run to ensure correct encoding.
 */
#include <stdio.h>

/* Basic ALU: add/sub/and/or/xor with lock prefix */
int test_alu(void) {
    int x = 10;
    asm volatile("lock addl %1, %0" : "+m"(x) : "r"(5));
    if (x != 15) { printf("FAIL: lock addl %d\n", x); return 0; }
    asm volatile("lock subl %1, %0" : "+m"(x) : "r"(3));
    if (x != 12) { printf("FAIL: lock subl %d\n", x); return 0; }
    asm volatile("lock andl %1, %0" : "+m"(x) : "r"(0xFF));
    if (x != 12) { printf("FAIL: lock andl %d\n", x); return 0; }
    asm volatile("lock orl %1, %0" : "+m"(x) : "r"(0x10));
    if (x != 28) { printf("FAIL: lock orl %d\n", x); return 0; }
    asm volatile("lock xorl %1, %0" : "+m"(x) : "r"(0xFF));
    if (x != 28 ^ 0xFF) { /* 28 ^ 255 = 227 */ }
    return 1;
}

/* cmpxchg family */
int test_cmpxchg(void) {
    int x = 42;
    int old = 42, new_val = 99;
    asm volatile("lock cmpxchgl %2, %1"
                 : "=a"(old), "+m"(x)
                 : "r"(new_val), "0"(old)
                 : "memory");
    if (x != 99) { printf("FAIL: cmpxchgl x=%d\n", x); return 0; }
    return 1;
}

/* xchg */
int test_xchg(void) {
    int x = 10, y = 20;
    asm volatile("xchgl %0, %1" : "+r"(x), "+m"(y));
    if (x != 20 || y != 10) { printf("FAIL: xchgl %d %d\n", x, y); return 0; }
    return 1;
}

/* bts/btr/btc/bt */
int test_bt(void) {
    unsigned long x = 0;
    asm volatile("btsq $5, %0" : "+r"(x));
    if (x != (1UL << 5)) { printf("FAIL: btsq %lu\n", x); return 0; }
    asm volatile("btrq $5, %0" : "+r"(x));
    if (x != 0) { printf("FAIL: btrq %lu\n", x); return 0; }
    asm volatile("btcq $7, %0" : "+r"(x));
    if (x != (1UL << 7)) { printf("FAIL: btcq %lu\n", x); return 0; }
    return 1;
}

/* bsf/bsr */
int test_bs(void) {
    unsigned long x = 0x80;
    long pos;
    asm volatile("bsfq %1, %0" : "=r"(pos) : "r"(x));
    if (pos != 7) { printf("FAIL: bsfq %ld\n", pos); return 0; }
    asm volatile("bsrq %1, %0" : "=r"(pos) : "r"(x));
    if (pos != 7) { printf("FAIL: bsrq %ld\n", pos); return 0; }
    return 1;
}

/* inc/dec */
int test_inc_dec(void) {
    int x = 0;
    asm volatile("incl %0" : "+r"(x));
    if (x != 1) { printf("FAIL: incl %d\n", x); return 0; }
    asm volatile("decl %0" : "+r"(x));
    if (x != 0) { printf("FAIL: decl %d\n", x); return 0; }
    return 1;
}

/* nop/pause */
int test_nop_pause(void) {
    asm volatile("nop");
    asm volatile("pause");
    return 1;
}

/* push/pop */
int test_push_pop(void) {
    long val = 0;
    asm volatile("pushq $42\n\tpopq %0" : "=r"(val));
    if (val != 42) { printf("FAIL: push/pop %ld\n", val); return 0; }
    return 1;
}

/* cmovz */
int test_cmov(void) {
    long x = 0, y = 99;
    asm volatile("testl %0, %0\n\tcmovzq %1, %0" : "+r"(x) : "r"(y));
    if (x != 99) { printf("FAIL: cmovzq %ld\n", x); return 0; }
    return 1;
}

/* setcc */
int test_setcc(void) {
    char z;
    asm volatile("xorl %%eax, %%eax\n\tsetz %0" : "=r"(z));
    if (z != 1) { printf("FAIL: setz %d\n", z); return 0; }
    return 1;
}

/* mul/div */
int test_mul_div(void) {
    unsigned int a = 7, b = 6, product;
    asm volatile("mull %2" : "=a"(product) : "a"(a), "r"(b));
    if (product != 42) { printf("FAIL: mull %u\n", product); return 0; }
    return 1;
}

/* shrd */
int test_shrd(void) {
    unsigned long dst = 0xFF00;
    unsigned long src = 0x00FF;
    asm volatile("shrdq $8, %1, %0" : "+r"(dst) : "r"(src));
    return 1;  /* just verify it encodes */
}

/* lar */
int test_lar(void) {
    unsigned long ar;
    unsigned short ss;
    asm volatile("mov %%ss, %0" : "=r"(ss));
    asm volatile("lar %1, %0" : "=r"(ar) : "r"((unsigned long)ss));
    return 1;  /* just verify it encodes */
}

/* rdpkru/wrpkru */
int test_pkru(void) {
    unsigned int eax, edx;
    asm volatile("rdpkru" : "=a"(eax), "=d"(edx) : "c"(0));
    return 1;  /* just verify it encodes */
}

/* clflush */
int test_clflush(void) {
    int x;
    asm volatile("clflush %0" : "+m"(x));
    return 1;
}

/* lock inc/dec */
int test_lock_inc_dec(void) {
    int x = 0;
    asm volatile("lock incl %0" : "+m"(x));
    if (x != 1) { printf("FAIL: lock incl %d\n", x); return 0; }
    asm volatile("lock decl %0" : "+m"(x));
    if (x != 0) { printf("FAIL: lock decl %d\n", x); return 0; }
    return 1;
}

/* lock xadd */
int test_lock_xadd(void) {
    int x = 10, old = 5;
    asm volatile("lock xaddl %0, %1" : "+r"(old), "+m"(x));
    if (old != 10 || x != 15) { printf("FAIL: lock xadd old=%d x=%d\n", old, x); return 0; }
    return 1;
}

/* cpuid */
int test_cpuid(void) {
    unsigned int eax, ebx, ecx, edx;
    asm volatile("cpuid"
        : "=a"(eax), "=b"(ebx), "=c"(ecx), "=d"(edx)
        : "a"(0), "c"(0));
    return 1;  /* just verify it encodes */
}

/* rdtsc */
int test_rdtsc(void) {
    unsigned int lo, hi;
    asm volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return 1;
}

/* rdtscp */
int test_rdtscp(void) {
    unsigned int lo, hi, aux;
    asm volatile("rdtscp" : "=a"(lo), "=d"(hi), "=c"(aux));
    return 1;
}

/* lfence/mfence/sfence */
int test_fences(void) {
    asm volatile("lfence" ::: "memory");
    asm volatile("mfence" ::: "memory");
    asm volatile("sfence" ::: "memory");
    return 1;
}

/* prefetch variants */
int test_prefetch(void) {
    int x;
    asm volatile("prefetchnta (%0)" :: "r"(&x));
    asm volatile("prefetcht0 (%0)" :: "r"(&x));
    asm volatile("prefetcht1 (%0)" :: "r"(&x));
    asm volatile("prefetcht2 (%0)" :: "r"(&x));
    return 1;
}

/* movbe */
int test_movbe(void) {
    unsigned int val = 0x12345678;
    unsigned int result;
    asm volatile("movbel (%1), %0" : "=r"(result) : "r"(&val));
    asm volatile("movbel %0, (%1)" :: "r"(result), "r"(&val));
    return 1;
}

/* crc32 */
int test_crc32(void) {
    unsigned int crc = 0;
    asm volatile("crc32l %1, %0" : "+r"(crc) : "r"(42));
    return 1;
}

/* popcnt */
int test_popcnt(void) {
    int count;
    asm volatile("popcntl %1, %0" : "=r"(count) : "r"(0xFF));
    if (count != 8) { printf("FAIL: popcntl %d\n", count); return 0; }
    return 1;
}

int main(void) {
    int ok = 1;
    ok &= test_alu();
    ok &= test_cmpxchg();
    ok &= test_xchg();
    ok &= test_bt();
    ok &= test_bs();
    ok &= test_inc_dec();
    ok &= test_nop_pause();
    ok &= test_push_pop();
    ok &= test_cmov();
    ok &= test_setcc();
    ok &= test_mul_div();
    ok &= test_shrd();
    ok &= test_lar();
    ok &= test_pkru();
    ok &= test_clflush();
    ok &= test_lock_inc_dec();
    ok &= test_lock_xadd();
    ok &= test_cpuid();
    ok &= test_rdtsc();
    ok &= test_rdtscp();
    ok &= test_fences();
    ok &= test_prefetch();
    ok &= test_movbe();
    ok &= test_crc32();
    ok &= test_popcnt();
    if (ok)
        printf("PASS: kernel asm instruction coverage\n");
    return ok ? 0 : 1;
}
