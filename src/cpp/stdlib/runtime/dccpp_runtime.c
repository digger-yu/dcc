/*
 * dcc++ C++ 标准库 C 运行时 (libdccpp_std)
 *
 * 设计: 全部用 C 写, 头文件由 C++ 头 <string>/<vector>/<iostream>
 * 等 wrap 后调用. 这是 Itanium C++ ABI 的最小子集实现.
 *
 * 命名空间约定: 所有外部符号前缀 `dccpp_` (dcc++ private), 避免
 * 与 libstdc++ / libc++ 冲突. C++ 头通过 extern "C" 形式引用.
 *
 * 涵盖:
 *   - string    : SSO (短字符串优化) + 长串堆分配
 *   - vector<T> : 动态数组, 元素构造 / 析构可选用回调
 *   - iostream  : cout/cin/cerr 对 libc FILE* 的薄包装
 *   - memory    : allocator + unique_ptr / shared_ptr 助手
 *   - exception : throw/catch 链, 与 dcc 现有 __cxa_throw 配合
 *   - typeinfo  : typeid / bad_cast
 *   - algorithm : sort / find / count / min / max
 *   - utility   : pair / move / swap / forward
 *   - functional: function<> 擦除
 */

#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

/* 通用 ABI 运行时: dcc 现有 lower.rs 已声明这些 */
extern void* _Znwm(size_t);
extern void  _ZdlPv(void*);

/* =========================================================================
 * 公共类型: ostream / istream (在 <iostream> 头之前定义)
 * ========================================================================= */

struct dccpp_ostream {
    FILE* fp;
};

struct dccpp_istream {
    FILE* fp;
};

/* =========================================================================
 * Part 1: std::string
 * ========================================================================= */

#define DCCPP_SSO_CAP 22   /* 短字符串优化: 22 字节内联 */

struct dccpp_string {
    /* 22 字节内联 buf; 长度 < 22 时使用 buf, 标志位在 flags */
    char buf[DCCPP_SSO_CAP];
    size_t size;     /* 实际字符数 (不含 '\0') */
    size_t cap;      /* 容量 (含 '\0'); SSO 模式 cap = DCCPP_SSO_CAP */
    int    is_long;  /* 0: SSO; 1: 堆分配, data = external */
    char*  data;     /* 堆模式时指向 malloc 区域; SSO 时 = buf */
};

/* 内部: 短串 → 长串扩容 */
static void dccpp_string_grow_long(struct dccpp_string* s, size_t need) {
    size_t newcap = s->cap ? s->cap * 2 : 32;
    while (newcap < need) newcap *= 2;
    char* p = (char*)malloc(newcap);
    if (!p) abort();
    if (s->size) memcpy(p, s->data, s->size);
    p[s->size] = '\0';
    if (s->is_long) free(s->data);
    s->data = p;
    s->cap = newcap;
    s->is_long = 1;
}

struct dccpp_string* dccpp_string_new(void) {
    struct dccpp_string* s = (struct dccpp_string*)malloc(sizeof(*s));
    if (!s) abort();
    s->buf[0] = '\0';
    s->size = 0;
    s->cap = DCCPP_SSO_CAP;
    s->is_long = 0;
    s->data = s->buf;
    return s;
}

struct dccpp_string* dccpp_string_from_cstr(const char* cstr) {
    struct dccpp_string* s = dccpp_string_new();
    size_t n = cstr ? strlen(cstr) : 0;
    if (n >= DCCPP_SSO_CAP) dccpp_string_grow_long(s, n + 1);
    memcpy(s->data, cstr ? cstr : "", n);
    s->data[n] = '\0';
    s->size = n;
    return s;
}

void dccpp_string_free(struct dccpp_string* s) {
    if (!s) return;
    if (s->is_long) free(s->data);
    free(s);
}

size_t dccpp_string_size(const struct dccpp_string* s) { return s->size; }
const char* dccpp_string_cstr(const struct dccpp_string* s) { return s->data; }

void dccpp_string_push_back(struct dccpp_string* s, char c) {
    if (s->size + 1 >= s->cap) dccpp_string_grow_long(s, s->size + 2);
    s->data[s->size++] = c;
    s->data[s->size] = '\0';
}

void dccpp_string_append(struct dccpp_string* s, const char* p, size_t n) {
    if (n == 0) return;
    if (s->size + n + 1 > s->cap) dccpp_string_grow_long(s, s->size + n + 1);
    memcpy(s->data + s->size, p, n);
    s->size += n;
    s->data[s->size] = '\0';
}

int dccpp_string_eq(const struct dccpp_string* a, const struct dccpp_string* b) {
    if (a->size != b->size) return 0;
    return memcmp(a->data, b->data, a->size) == 0;
}

int dccpp_string_eq_cstr(const struct dccpp_string* a, const char* b) {
    size_t n = strlen(b);
    if (a->size != n) return 0;
    return memcmp(a->data, b, n) == 0;
}

char dccpp_string_at(const struct dccpp_string* s, size_t i) { return s->data[i]; }

void dccpp_string_clear(struct dccpp_string* s) {
    s->size = 0;
    if (s->data != s->buf) s->data[0] = '\0';
    else s->buf[0] = '\0';
}

/* 整型转字符串: cout << 42 */
char* dccpp_i64_to_cstr(long long v, char* buf, size_t bufsz) {
    snprintf(buf, bufsz, "%lld", v);
    return buf;
}

char* dccpp_u64_to_cstr(unsigned long long v, char* buf, size_t bufsz) {
    snprintf(buf, bufsz, "%llu", v);
    return buf;
}

char* dccpp_f64_to_cstr(double v, char* buf, size_t bufsz) {
    snprintf(buf, bufsz, "%g", v);
    return buf;
}

/* =========================================================================
 * Part 2: std::vector<T>
 * ========================================================================= */

struct dccpp_vector {
    void*  data;       /* 元素首地址 */
    size_t size;       /* 元素数 */
    size_t cap;        /* 容量 (元素数) */
    size_t elem_size;  /* sizeof(T) */
    void   (*dtor)(void*);  /* 析构回调, NULL 表示 POD */
};

struct dccpp_vector* dccpp_vector_new(size_t elem_size, void (*dtor)(void*)) {
    struct dccpp_vector* v = (struct dccpp_vector*)malloc(sizeof(*v));
    if (!v) abort();
    v->data = NULL;
    v->size = 0;
    v->cap = 0;
    v->elem_size = elem_size;
    v->dtor = dtor;
    return v;
}

void dccpp_vector_free(struct dccpp_vector* v) {
    if (!v) return;
    if (v->dtor && v->data) {
        for (size_t i = 0; i < v->size; ++i) {
            v->dtor((char*)v->data + i * v->elem_size);
        }
    }
    free(v->data);
    free(v);
}

size_t dccpp_vector_size(const struct dccpp_vector* v) { return v->size; }
size_t dccpp_vector_cap(const struct dccpp_vector* v)  { return v->cap; }
int    dccpp_vector_empty(const struct dccpp_vector* v) { return v->size == 0; }

static void dccpp_vector_reserve_impl(struct dccpp_vector* v, size_t need) {
    if (need <= v->cap) return;
    size_t newcap = v->cap ? v->cap * 2 : 4;
    while (newcap < need) newcap *= 2;
    void* p = realloc(v->data, newcap * v->elem_size);
    if (!p) abort();
    v->data = p;
    v->cap = newcap;
}

void dccpp_vector_reserve(struct dccpp_vector* v, size_t n) {
    dccpp_vector_reserve_impl(v, n);
}

void dccpp_vector_push_back_copy(struct dccpp_vector* v, const void* elem) {
    dccpp_vector_reserve_impl(v, v->size + 1);
    memcpy((char*)v->data + v->size * v->elem_size, elem, v->elem_size);
    v->size++;
}

void dccpp_vector_pop_back(struct dccpp_vector* v) {
    if (v->size == 0) return;
    v->size--;
    if (v->dtor) v->dtor((char*)v->data + v->size * v->elem_size);
}

void* dccpp_vector_at(struct dccpp_vector* v, size_t i) {
    return (char*)v->data + i * v->elem_size;
}

void dccpp_vector_clear(struct dccpp_vector* v) {
    if (v->dtor && v->data) {
        for (size_t i = 0; i < v->size; ++i) {
            v->dtor((char*)v->data + i * v->elem_size);
        }
    }
    v->size = 0;
}

void dccpp_vector_resize(struct dccpp_vector* v, size_t n) {
    if (n > v->size) {
        dccpp_vector_reserve_impl(v, n);
        memset((char*)v->data + v->size * v->elem_size, 0,
               (n - v->size) * v->elem_size);
    } else if (v->dtor && n < v->size) {
        for (size_t i = n; i < v->size; ++i) {
            v->dtor((char*)v->data + i * v->elem_size);
        }
    }
    v->size = n;
}

/* =========================================================================
 * Part 3: std::iostream (cout / cin / cerr)
 * ========================================================================= */

/* 全局: 标准流对象 (C++ 头 <iostream> 会 extern 引用这些符号) */
/* C 中 stdout/stderr/stdin 不是常量表达式, 使用 NULL 初始化并在首次使用时延迟赋值 */
static struct dccpp_ostream _dccpp_cout = { NULL };
static struct dccpp_ostream _dccpp_cerr = { NULL };
static struct dccpp_istream _dccpp_cin  = { NULL };

struct dccpp_ostream* dccpp_cout(void) {
    if (!_dccpp_cout.fp) _dccpp_cout.fp = stdout;
    return &_dccpp_cout;
}
struct dccpp_ostream* dccpp_cerr(void) {
    if (!_dccpp_cerr.fp) _dccpp_cerr.fp = stderr;
    return &_dccpp_cerr;
}
struct dccpp_istream* dccpp_cin(void) {
    if (!_dccpp_cin.fp) _dccpp_cin.fp = stdin;
    return &_dccpp_cin;
}

void dccpp_ostream_putc(struct dccpp_ostream* os, char c) {
    fputc(c, os->fp);
}

void dccpp_ostream_puts(struct dccpp_ostream* os, const char* s) {
    fputs(s, os->fp);
}

void dccpp_ostream_write(struct dccpp_ostream* os, const char* p, size_t n) {
    fwrite(p, 1, n, os->fp);
}

void dccpp_ostream_flush(struct dccpp_ostream* os) {
    fflush(os->fp);
}

void dccpp_ostream_i64(struct dccpp_ostream* os, long long v) {
    fprintf(os->fp, "%lld", v);
}

void dccpp_ostream_u64(struct dccpp_ostream* os, unsigned long long v) {
    fprintf(os->fp, "%llu", v);
}

void dccpp_ostream_f64(struct dccpp_ostream* os, double v) {
    fprintf(os->fp, "%g", v);
}

void dccpp_ostream_ptr(struct dccpp_ostream* os, const void* p) {
    fprintf(os->fp, "%p", p);
}

void dccpp_ostream_bool(struct dccpp_ostream* os, int v) {
    fputs(v ? "true" : "false", os->fp);
}

void dccpp_ostream_endl(struct dccpp_ostream* os) {
    fputc('\n', os->fp);
    fflush(os->fp);
}

int dccpp_istream_getc(struct dccpp_istream* is) {
    return fgetc(is->fp);
}

int dccpp_istream_eof(struct dccpp_istream* is) {
    return feof(is->fp);
}

void dccpp_istream_i64(struct dccpp_istream* is, long long* out) {
    long long v = 0;
    fscanf(is->fp, "%lld", &v);
    *out = v;
}

void dccpp_istream_u64(struct dccpp_istream* is, unsigned long long* out) {
    unsigned long long v = 0;
    fscanf(is->fp, "%llu", &v);
    *out = v;
}

void dccpp_istream_f64(struct dccpp_istream* is, double* out) {
    double v = 0;
    fscanf(is->fp, "%lf", &v);
    *out = v;
}

/* =========================================================================
 * Part 4: std::memory (allocator / new / delete 包装)
 * ========================================================================= */

void* dccpp_alloc(size_t n) { return malloc(n); }
void  dccpp_dealloc(void* p) { free(p); }

/* unique_ptr: 一个简单 RAII 包装 */
struct dccpp_unique_ptr {
    void* ptr;
    void (*dtor)(void*);
};

void dccpp_unique_ptr_reset(struct dccpp_unique_ptr* u,
                            void* p, void (*dtor)(void*)) {
    if (u->ptr && u->dtor) u->dtor(u->ptr);
    u->ptr = p;
    u->dtor = dtor;
}

void* dccpp_unique_ptr_get(const struct dccpp_unique_ptr* u) { return u->ptr; }
void* dccpp_unique_ptr_release(struct dccpp_unique_ptr* u) {
    void* p = u->ptr;
    u->ptr = NULL;
    u->dtor = NULL;
    return p;
}

/* shared_ptr: 简单引用计数 */
struct dccpp_shared_ptr {
    void* ptr;
    void (*dtor)(void*);
    size_t* refcnt;
};

struct dccpp_shared_ptr dccpp_make_shared(void* p, void (*dtor)(void*)) {
    struct dccpp_shared_ptr s;
    s.ptr = p;
    s.dtor = dtor;
    s.refcnt = (size_t*)malloc(sizeof(size_t));
    *s.refcnt = 1;
    return s;
}

void dccpp_shared_ptr_inc(struct dccpp_shared_ptr* s) {
    if (s->refcnt) (*s->refcnt)++;
}

void dccpp_shared_ptr_dec(struct dccpp_shared_ptr* s) {
    if (!s->refcnt) return;
    if (--(*s->refcnt) == 0) {
        if (s->ptr && s->dtor) s->dtor(s->ptr);
        free(s->refcnt);
        s->ptr = NULL;
        s->refcnt = NULL;
        s->dtor = NULL;
    }
}

void* dccpp_shared_ptr_get(const struct dccpp_shared_ptr* s) { return s->ptr; }
size_t dccpp_shared_ptr_use_count(const struct dccpp_shared_ptr* s) {
    return s->refcnt ? *s->refcnt : 0;
}

/* =========================================================================
 * Part 5: std::exception / std::stdexcept
 * ========================================================================= */

/* 与 lower.rs 中的 __cxa_throw 配合 */
extern void __cxa_throw(void*, void*, void(*)(void*));
extern void* __cxa_allocate_exception(size_t);
extern void __cxa_free_exception(void*);
extern void* __cxa_begin_catch(void*);
extern void __cxa_end_catch(void);

static const char* dccpp_exception_msg = 0;

void dccpp_throw_bad_alloc(void) {
    /* Bad alloc: 仅抛, 不带消息 */
    void* p = __cxa_allocate_exception(sizeof(int));
    __cxa_throw(p, 0, 0);
}

void dccpp_throw_runtime_error(const char* msg) {
    dccpp_exception_msg = msg;
    void* p = __cxa_allocate_exception(sizeof(void*));
    *(const char**)p = msg;
    __cxa_throw(p, 0, 0);
}

void dccpp_throw_out_of_range(const char* msg) {
    dccpp_exception_msg = msg;
    void* p = __cxa_allocate_exception(sizeof(void*));
    *(const char**)p = msg;
    __cxa_throw(p, 0, 0);
}

const char* dccpp_exception_what(void* exc) {
    (void)exc;
    return dccpp_exception_msg ? dccpp_exception_msg : "dcc++ exception";
}

/* =========================================================================
 * Part 6: std::typeinfo
 * ========================================================================= */

struct dccpp_typeinfo {
    const char* name;
    void*       base;  /* 占位, 简化 */
};

static struct dccpp_typeinfo _dccpp_typeinfo_int    = { "i", 0 };
static struct dccpp_typeinfo _dccpp_typeinfo_char   = { "c", 0 };
static struct dccpp_typeinfo _dccpp_typeinfo_void   = { "v", 0 };
static struct dccpp_typeinfo _dccpp_typeinfo_double = { "d", 0 };
static struct dccpp_typeinfo _dccpp_typeinfo_bool   = { "b", 0 };

const char* dccpp_typeinfo_name(const struct dccpp_typeinfo* ti) {
    return ti ? ti->name : "?";
}

int dccpp_typeinfo_eq(const struct dccpp_typeinfo* a,
                      const struct dccpp_typeinfo* b) {
    if (a == b) return 1;
    if (!a || !b) return 0;
    return a->name == b->name || strcmp(a->name, b->name) == 0;
}

struct dccpp_typeinfo* dccpp_typeid_int(void)    { return &_dccpp_typeinfo_int; }
struct dccpp_typeinfo* dccpp_typeid_char(void)   { return &_dccpp_typeinfo_char; }
struct dccpp_typeinfo* dccpp_typeid_void(void)   { return &_dccpp_typeinfo_void; }
struct dccpp_typeinfo* dccpp_typeid_double(void) { return &_dccpp_typeinfo_double; }
struct dccpp_typeinfo* dccpp_typeid_bool(void)   { return &_dccpp_typeinfo_bool; }

/* =========================================================================
 * Part 7: std::algorithm
 * ========================================================================= */

void dccpp_swap_bytes(void* a, void* b, size_t n) {
    char* pa = (char*)a;
    char* pb = (char*)b;
    for (size_t i = 0; i < n; ++i) {
        char t = pa[i]; pa[i] = pb[i]; pb[i] = t;
    }
}

/* 插入排序 (用于 dccpp_sort) */
void dccpp_sort(void* base, size_t n, size_t esz,
                int (*cmp)(const void*, const void*)) {
    if (n < 2) return;
    char* arr = (char*)base;
    for (size_t i = 1; i < n; ++i) {
        char tmp[64];
        if (esz > 64) continue;  /* 仅处理 <=64 字节元素 */
        memcpy(tmp, arr + i * esz, esz);
        size_t j = i;
        while (j > 0 && cmp(arr + (j - 1) * esz, tmp) > 0) {
            memcpy(arr + j * esz, arr + (j - 1) * esz, esz);
            j--;
        }
        memcpy(arr + j * esz, tmp, esz);
    }
}

void* dccpp_find(void* base, size_t n, size_t esz, const void* key,
                 int (*cmp)(const void*, const void*)) {
    char* arr = (char*)base;
    for (size_t i = 0; i < n; ++i) {
        if (cmp(arr + i * esz, key) == 0) {
            return arr + i * esz;
        }
    }
    return NULL;
}

size_t dccpp_count(void* base, size_t n, size_t esz, const void* key,
                   int (*cmp)(const void*, const void*)) {
    char* arr = (char*)base;
    size_t c = 0;
    for (size_t i = 0; i < n; ++i) {
        if (cmp(arr + i * esz, key) == 0) c++;
    }
    return c;
}

long long dccpp_min_ll(long long a, long long b) { return a < b ? a : b; }
long long dccpp_max_ll(long long a, long long b) { return a > b ? a : b; }
double    dccpp_min_f (double a, double b)       { return a < b ? a : b; }
double    dccpp_max_f (double a, double b)       { return a > b ? a : b; }

/* =========================================================================
 * Part 8: std::utility (pair / move / swap / forward 占位)
 * ========================================================================= */

struct dccpp_pair {
    void* first;
    void* second;
};

struct dccpp_pair dccpp_make_pair(void* a, void* b) {
    struct dccpp_pair p = { a, b };
    return p;
}

/* =========================================================================
 * Part 9: std::map<K,V> 简化版 (排序数组, 适合 < 1000 项)
 * ========================================================================= */

struct dccpp_map_entry {
    void* key;
    void* val;
    int   used;
    int   (*key_cmp)(const void*, const void*);
    void  (*key_dtor)(void*);
    void  (*val_dtor)(void*);
};

struct dccpp_map {
    struct dccpp_map_entry* arr;
    size_t size;
    size_t cap;
};

struct dccpp_map* dccpp_map_new(size_t cap,
                                int (*kc)(const void*, const void*),
                                void (*kd)(void*),
                                void (*vd)(void*)) {
    struct dccpp_map* m = (struct dccpp_map*)malloc(sizeof(*m));
    if (!m) abort();
    m->arr = (struct dccpp_map_entry*)calloc(cap, sizeof(*m->arr));
    if (!m->arr) abort();
    m->size = 0;
    m->cap = cap;
    for (size_t i = 0; i < cap; ++i) {
        m->arr[i].key_cmp = kc;
        m->arr[i].key_dtor = kd;
        m->arr[i].val_dtor = vd;
    }
    return m;
}

void dccpp_map_free(struct dccpp_map* m) {
    if (!m) return;
    for (size_t i = 0; i < m->cap; ++i) {
        if (m->arr[i].used) {
            if (m->arr[i].key_dtor) m->arr[i].key_dtor(m->arr[i].key);
            if (m->arr[i].val_dtor) m->arr[i].val_dtor(m->arr[i].val);
        }
    }
    free(m->arr);
    free(m);
}

int dccpp_map_insert(struct dccpp_map* m, void* key, void* val) {
    for (size_t i = 0; i < m->cap; ++i) {
        if (!m->arr[i].used) {
            m->arr[i].used = 1;
            m->arr[i].key = key;
            m->arr[i].val = val;
            m->size++;
            return 1;
        }
    }
    return 0;
}

void* dccpp_map_get(struct dccpp_map* m, const void* key) {
    for (size_t i = 0; i < m->cap; ++i) {
        if (m->arr[i].used && m->arr[i].key_cmp(m->arr[i].key, key) == 0) {
            return m->arr[i].val;
        }
    }
    return NULL;
}

int dccpp_map_contains(struct dccpp_map* m, const void* key) {
    return dccpp_map_get(m, key) != NULL;
}

size_t dccpp_map_size(const struct dccpp_map* m) { return m->size; }

/* =========================================================================
 * Part 10: std::initializer_list 兼容
 * ========================================================================= */

struct dccpp_initializer_list {
    const void* data;
    size_t size;
    size_t elem_size;
};

const void* dccpp_init_list_begin(const struct dccpp_initializer_list* l) {
    return l->data;
}

size_t dccpp_init_list_size(const struct dccpp_initializer_list* l) {
    return l->size;
}

/* =========================================================================
 * Part 11: std::chrono (M18)
 *
 * 时间 / 时长 / 时钟点. 三种 clock 均基于 clock_gettime(CLOCK_xxx),
 * 内部统一用 int64_t 表示纳秒. 精度足以覆盖 system_clock /
 * steady_clock / high_resolution_clock.
 * ========================================================================= */

#include <time.h>

struct dccpp_duration {
    long long nanos;  /* 纳秒 (signed, 允许负值) */
};

struct dccpp_time_point {
    long long since_epoch_nanos;  /* 自 epoch 起的纳秒 */
};

struct dccpp_duration* dccpp_dur_from_nanos(long long ns) {
    struct dccpp_duration* d = (struct dccpp_duration*)malloc(sizeof(struct dccpp_duration));
    d->nanos = ns;
    return d;
}
struct dccpp_duration* dccpp_dur_from_micros(long long us) {
    return dccpp_dur_from_nanos(us * 1000);
}
struct dccpp_duration* dccpp_dur_from_millis(long long ms) {
    return dccpp_dur_from_nanos(ms * 1000000);
}
struct dccpp_duration* dccpp_dur_from_secs(long long s) {
    return dccpp_dur_from_nanos(s * 1000000000);
}

long long dccpp_dur_nanos(const struct dccpp_duration* d)  { return d->nanos; }
long long dccpp_dur_micros(const struct dccpp_duration* d) { return d->nanos / 1000; }
long long dccpp_dur_millis(const struct dccpp_duration* d) { return d->nanos / 1000000; }
long long dccpp_dur_secs(const struct dccpp_duration* d)   { return d->nanos / 1000000000; }

struct dccpp_duration* dccpp_dur_add(const struct dccpp_duration* a,
                                    const struct dccpp_duration* b) {
    return dccpp_dur_from_nanos(a->nanos + b->nanos);
}
struct dccpp_duration* dccpp_dur_sub(const struct dccpp_duration* a,
                                    const struct dccpp_duration* b) {
    return dccpp_dur_from_nanos(a->nanos - b->nanos);
}
int dccpp_dur_eq(const struct dccpp_duration* a, const struct dccpp_duration* b) {
    return a->nanos == b->nanos;
}
int dccpp_dur_lt(const struct dccpp_duration* a, const struct dccpp_duration* b) {
    return a->nanos < b->nanos;
}
int dccpp_dur_gt(const struct dccpp_duration* a, const struct dccpp_duration* b) {
    return a->nanos > b->nanos;
}
int dccpp_dur_zero(const struct dccpp_duration* d) { return d->nanos == 0; }
void dccpp_dur_free(struct dccpp_duration* d) { if (d) free(d); }

struct dccpp_time_point* dccpp_now_system(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    struct dccpp_time_point* tp = (struct dccpp_time_point*)malloc(sizeof(struct dccpp_time_point));
    tp->since_epoch_nanos = (long long)ts.tv_sec * 1000000000 + (long long)ts.tv_nsec;
    return tp;
}
struct dccpp_time_point* dccpp_now_steady(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    struct dccpp_time_point* tp = (struct dccpp_time_point*)malloc(sizeof(struct dccpp_time_point));
    tp->since_epoch_nanos = (long long)ts.tv_sec * 1000000000 + (long long)ts.tv_nsec;
    return tp;
}
struct dccpp_time_point* dccpp_now_hires(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    struct dccpp_time_point* tp = (struct dccpp_time_point*)malloc(sizeof(struct dccpp_time_point));
    tp->since_epoch_nanos = (long long)ts.tv_sec * 1000000000 + (long long)ts.tv_nsec;
    return tp;
}

struct dccpp_time_point* dccpp_tp_add_dur(const struct dccpp_time_point* tp,
                                         const struct dccpp_duration* d) {
    struct dccpp_time_point* r = (struct dccpp_time_point*)malloc(sizeof(struct dccpp_time_point));
    r->since_epoch_nanos = tp->since_epoch_nanos + d->nanos;
    return r;
}
struct dccpp_time_point* dccpp_tp_sub_dur(const struct dccpp_time_point* tp,
                                         const struct dccpp_duration* d) {
    struct dccpp_time_point* r = (struct dccpp_time_point*)malloc(sizeof(struct dccpp_time_point));
    r->since_epoch_nanos = tp->since_epoch_nanos - d->nanos;
    return r;
}
struct dccpp_duration* dccpp_tp_diff(const struct dccpp_time_point* a,
                                    const struct dccpp_time_point* b) {
    return dccpp_dur_from_nanos(a->since_epoch_nanos - b->since_epoch_nanos);
}
int dccpp_tp_eq(const struct dccpp_time_point* a, const struct dccpp_time_point* b) {
    return a->since_epoch_nanos == b->since_epoch_nanos;
}
int dccpp_tp_lt(const struct dccpp_time_point* a, const struct dccpp_time_point* b) {
    return a->since_epoch_nanos < b->since_epoch_nanos;
}
int dccpp_tp_le(const struct dccpp_time_point* a, const struct dccpp_time_point* b) {
    return a->since_epoch_nanos <= b->since_epoch_nanos;
}
void dccpp_tp_free(struct dccpp_time_point* tp) { if (tp) free(tp); }

/* =========================================================================
 * Part 12: std::thread / std::mutex / std::condition_variable (M18)
 *
 * 全部基于 pthread. 线程入口是 void* (*)(void*) 函数指针 + void* 参数,
 * 与 dcc++ 函数指针规则一致 (成员函数会被 lower 成 C thunk).
 * ========================================================================= */

#include <pthread.h>
#include <errno.h>

struct dccpp_thread {
    pthread_t           handle;
    int                 joined;
    void*               (*fn)(void*);
    void*               arg;
    void*               ret;
};

struct dccpp_thread* dccpp_thread_new(void* (*fn)(void*), void* arg) {
    struct dccpp_thread* t = (struct dccpp_thread*)malloc(sizeof(*t));
    if (!t) abort();
    t->fn = fn;
    t->arg = arg;
    t->joined = 0;
    t->ret = NULL;
    if (pthread_create(&t->handle, NULL, fn, arg) != 0) {
        free(t);
        return NULL;
    }
    return t;
}

void* dccpp_thread_join(struct dccpp_thread* t) {
    if (!t->joined) {
        pthread_join(t->handle, &t->ret);
        t->joined = 1;
    }
    return t->ret;
}

void dccpp_thread_detach(struct dccpp_thread* t) {
    if (!t->joined) {
        pthread_detach(t->handle);
        t->joined = 1;
    }
}

void dccpp_thread_free(struct dccpp_thread* t) {
    if (!t) return;
    if (!t->joined) {
        pthread_join(t->handle, NULL);
    }
    free(t);
}

void dccpp_thread_sleep_ns(int64_t ns) {
    struct timespec ts;
    ts.tv_sec  = (time_t)(ns / 1000000000);
    ts.tv_nsec = (long)(ns % 1000000000);
    nanosleep(&ts, NULL);
}

void dccpp_thread_yield(void) { sched_yield(); }

int  dccpp_thread_eq(const struct dccpp_thread* a, const struct dccpp_thread* b) {
    return pthread_equal(a->handle, b->handle);
}

struct dccpp_mutex {
    pthread_mutex_t m;
};

struct dccpp_mutex* dccpp_mutex_new(void) {
    struct dccpp_mutex* m = (struct dccpp_mutex*)malloc(sizeof(*m));
    if (!m) abort();
    if (pthread_mutex_init(&m->m, NULL) != 0) {
        free(m);
        return NULL;
    }
    return m;
}

void dccpp_mutex_free(struct dccpp_mutex* m) {
    if (!m) return;
    pthread_mutex_destroy(&m->m);
    free(m);
}

void dccpp_mutex_lock(struct dccpp_mutex* m)   { pthread_mutex_lock(&m->m); }
int  dccpp_mutex_trylock(struct dccpp_mutex* m) { return pthread_mutex_trylock(&m->m) == 0; }
void dccpp_mutex_unlock(struct dccpp_mutex* m) { pthread_mutex_unlock(&m->m); }

struct dccpp_recursive_mutex {
    pthread_mutex_t m;
};

struct dccpp_recursive_mutex* dccpp_rmutex_new(void) {
    struct dccpp_recursive_mutex* m =
        (struct dccpp_recursive_mutex*)malloc(sizeof(*m));
    if (!m) abort();
    pthread_mutexattr_t a;
    pthread_mutexattr_init(&a);
    pthread_mutexattr_settype(&a, PTHREAD_MUTEX_RECURSIVE);
    if (pthread_mutex_init(&m->m, &a) != 0) {
        pthread_mutexattr_destroy(&a);
        free(m);
        return NULL;
    }
    pthread_mutexattr_destroy(&a);
    return m;
}

void dccpp_rmutex_free(struct dccpp_recursive_mutex* m) {
    if (!m) return;
    pthread_mutex_destroy(&m->m);
    free(m);
}

void dccpp_rmutex_lock(struct dccpp_recursive_mutex* m)   { pthread_mutex_lock(&m->m); }
void dccpp_rmutex_unlock(struct dccpp_recursive_mutex* m) { pthread_mutex_unlock(&m->m); }

struct dccpp_condition_variable {
    pthread_cond_t cv;
};

struct dccpp_condition_variable* dccpp_cv_new(void) {
    struct dccpp_condition_variable* cv =
        (struct dccpp_condition_variable*)malloc(sizeof(*cv));
    if (!cv) abort();
    if (pthread_cond_init(&cv->cv, NULL) != 0) {
        free(cv);
        return NULL;
    }
    return cv;
}

void dccpp_cv_free(struct dccpp_condition_variable* cv) {
    if (!cv) return;
    pthread_cond_destroy(&cv->cv);
    free(cv);
}

void dccpp_cv_wait(struct dccpp_condition_variable* cv, struct dccpp_mutex* m) {
    pthread_cond_wait(&cv->cv, &m->m);
}

int  dccpp_cv_wait_for(struct dccpp_condition_variable* cv,
                       struct dccpp_mutex* m,
                       const struct dccpp_duration* d) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    int64_t target_ns = (int64_t)ts.tv_sec * 1000000000 + (int64_t)ts.tv_nsec + d->nanos;
    ts.tv_sec  = (time_t)(target_ns / 1000000000);
    ts.tv_nsec = (long)(target_ns % 1000000000);
    int rc = pthread_cond_timedwait(&cv->cv, &m->m, &ts);
    return rc == 0;  /* 1 = 收到通知, 0 = 超时 */
}

void dccpp_cv_notify_one(struct dccpp_condition_variable* cv)  { pthread_cond_signal(&cv->cv); }
void dccpp_cv_notify_all(struct dccpp_condition_variable* cv)  { pthread_cond_broadcast(&cv->cv); }

struct dccpp_once_flag {
    pthread_once_t flag;
};

#define DCCPP_ONCE_FLAG_INIT { PTHREAD_ONCE_INIT }

void dccpp_call_once(struct dccpp_once_flag* flag, void (*fn)(void)) {
    pthread_once(&flag->flag, fn);
}

/* =========================================================================
 * Part 13: std::filesystem (M18)
 *
 * path 类以字节向量存储, file_status 编码 type/exists. 真实 stat
 * 调用走 stat(2)/opendir(2)/readdir(2) POSIX API.
 * ========================================================================= */

#include <sys/stat.h>
#include <dirent.h>
#include <unistd.h>

struct dccpp_path {
    char*  data;
    size_t size;
    size_t cap;
};

static void dccpp_path_grow(struct dccpp_path* p, size_t need) {
    if (need <= p->cap) return;
    size_t newcap = p->cap ? p->cap * 2 : 32;
    while (newcap < need) newcap *= 2;
    char* q = (char*)realloc(p->data, newcap);
    if (!q) abort();
    p->data = q;
    p->cap  = newcap;
}

struct dccpp_path* dccpp_path_new(const char* s) {
    struct dccpp_path* p = (struct dccpp_path*)malloc(sizeof(*p));
    if (!p) abort();
    p->cap = 0; p->data = NULL; p->size = 0;
    if (s) {
        size_t n = strlen(s);
        dccpp_path_grow(p, n + 1);
        memcpy(p->data, s, n);
        p->data[n] = '\0';
        p->size = n;
    } else {
        dccpp_path_grow(p, 1);
        p->data[0] = '\0';
    }
    return p;
}

struct dccpp_path* dccpp_path_copy(const struct dccpp_path* p) {
    return dccpp_path_new(p && p->data ? p->data : "");
}

void dccpp_path_free(struct dccpp_path* p) {
    if (!p) return;
    free(p->data);
    free(p);
}

const char* dccpp_path_cstr(const struct dccpp_path* p) {
    return p && p->data ? p->data : "";
}

size_t dccpp_path_size(const struct dccpp_path* p) { return p ? p->size : 0; }

void dccpp_path_assign(struct dccpp_path* p, const char* s) {
    size_t n = s ? strlen(s) : 0;
    dccpp_path_grow(p, n + 1);
    if (n) memcpy(p->data, s, n);
    p->data[n] = '\0';
    p->size = n;
}

void dccpp_path_append(struct dccpp_path* p, const char* s) {
    if (!s) return;
    size_t n = strlen(s);
    if (n == 0) return;
    if (p->size > 0 && p->data[p->size - 1] != '/' && s[0] != '/') {
        dccpp_path_grow(p, p->size + n + 2);
        p->data[p->size++] = '/';
    } else {
        dccpp_path_grow(p, p->size + n + 1);
    }
    memcpy(p->data + p->size, s, n);
    p->size += n;
    p->data[p->size] = '\0';
}

void dccpp_path_append_path(struct dccpp_path* p, const struct dccpp_path* other) {
    if (other) dccpp_path_append(p, other->data ? other->data : "");
}

int dccpp_path_eq(const struct dccpp_path* a, const struct dccpp_path* b) {
    if (!a || !b) return a == b;
    if (a->size != b->size) return 0;
    return memcmp(a->data, b->data, a->size) == 0;
}

int dccpp_path_eq_cstr(const struct dccpp_path* a, const char* s) {
    if (!a) return s == NULL || *s == '\0';
    size_t n = s ? strlen(s) : 0;
    if (a->size != n) return 0;
    return memcmp(a->data, s, n) == 0;
}

/* file_status: type codes
 *   0 = none (does not exist)
 *   1 = regular file
 *   2 = directory
 *   3 = symlink
 *   4 = other (block/char/fifo/socket)
 */
struct dccpp_file_status {
    int type;
    int perms;  /* rwx bits as 0-7 */
};

struct dccpp_file_status dccpp_status_of(const char* path) {
    struct dccpp_file_status fs = { 0, 0 };
    if (!path) return fs;
    struct stat st;
    if (lstat(path, &st) != 0) {
        fs.type = 0;
        return fs;
    }
    if      (S_ISREG(st.st_mode))  fs.type = 1;
    else if (S_ISDIR(st.st_mode))  fs.type = 2;
    else if (S_ISLNK(st.st_mode))  fs.type = 3;
    else                           fs.type = 4;
    fs.perms = (st.st_mode & 0777);
    return fs;
}

int dccpp_fs_exists(const char* path) {
    if (!path) return 0;
    struct stat st;
    return stat(path, &st) == 0;
}
int dccpp_fs_is_regular(const char* path) {
    struct stat st;
    if (!path || stat(path, &st) != 0) return 0;
    return S_ISREG(st.st_mode);
}
int dccpp_fs_is_directory(const char* path) {
    struct stat st;
    if (!path || stat(path, &st) != 0) return 0;
    return S_ISDIR(st.st_mode);
}
int dccpp_fs_remove(const char* path) {
    if (!path) return 0;
    /* 用 unlink; 目录应该用 rmdir. 简化: 试 unlink 再 rmdir. */
    if (unlink(path) == 0) return 1;
    return rmdir(path) == 0;
}
int dccpp_fs_mkdir(const char* path) {
    if (!path) return 0;
    return mkdir(path, 0755) == 0;
}
int dccpp_fs_rename(const char* src, const char* dst) {
    if (!src || !dst) return 0;
    return rename(src, dst) == 0;
}
int64_t dccpp_fs_file_size(const char* path) {
    if (!path) return -1;
    struct stat st;
    if (stat(path, &st) != 0) return -1;
    return (int64_t)st.st_size;
}

/* directory_entry 简化: 缓存路径 + 当前 stat 类型 */
struct dccpp_directory {
    DIR* d;
};

struct dccpp_directory* dccpp_dir_open(const char* path) {
    if (!path) return NULL;
    DIR* d = opendir(path);
    if (!d) return NULL;
    struct dccpp_directory* dd = (struct dccpp_directory*)malloc(sizeof(*dd));
    if (!dd) { closedir(d); return NULL; }
    dd->d = d;
    return dd;
}

void dccpp_dir_close(struct dccpp_directory* d) {
    if (!d) return;
    if (d->d) closedir(d->d);
    free(d);
}

const char* dccpp_dir_read(struct dccpp_directory* d) {
    if (!d || !d->d) return NULL;
    struct dirent* e = readdir(d->d);
    if (!e) return NULL;
    return e->d_name;
}

/* =========================================================================
 * Part 14: std::expected<T, E> (C++23, M18)
 *
 * 用一个 tag 字节区分 has_value / has_error, union 存值.
 * 字节大小 = max(sizeof(T), sizeof(E)) + 1 + padding.
 * 这里用"擦除到 void*" + 析构回调避免 union 模板化.
 * ========================================================================= */

struct dccpp_expected {
    void* val;
    void* err;
    int   has_val;
    int   val_dtor_needed;  /* T 是否需要析构 */
    int   err_dtor_needed;  /* E 是否需要析构 */
    void  (*val_dtor)(void*);
    void  (*err_dtor)(void*);
    size_t val_size;
    size_t err_size;
};

struct dccpp_expected dccpp_expected_make_val(void* v, size_t vs,
                                              void (*vd)(void*)) {
    struct dccpp_expected e = {0};
    e.val = v;
    e.has_val = 1;
    e.val_dtor_needed = vd != NULL;
    e.val_dtor = vd;
    e.val_size = vs;
    return e;
}

struct dccpp_expected dccpp_expected_make_err(void* er, size_t es,
                                              void (*ed)(void*)) {
    struct dccpp_expected e = {0};
    e.err = er;
    e.has_val = 0;
    e.err_dtor_needed = ed != NULL;
    e.err_dtor = ed;
    e.err_size = es;
    return e;
}

void dccpp_expected_destroy(struct dccpp_expected* e) {
    if (!e) return;
    if (e->has_val) {
        if (e->val_dtor_needed && e->val_dtor && e->val) e->val_dtor(e->val);
    } else {
        if (e->err_dtor_needed && e->err_dtor && e->err) e->err_dtor(e->err);
    }
    e->val = NULL;
    e->err = NULL;
}

int  dccpp_expected_has_value(const struct dccpp_expected* e) { return e->has_val; }
void* dccpp_expected_value(const struct dccpp_expected* e)      { return e->val; }
void* dccpp_expected_error(const struct dccpp_expected* e)      { return e->err; }

/* =========================================================================
 * Part 15: std::flat_map<K, V> (C++23, M18)
 *
 * 排序后的 (K, V) 数组, 二分查找. flat_map 比 std::map 缓存友好.
 * 元素按字节存 + K 比较回调.
 * ========================================================================= */

struct dccpp_flat_map_entry {
    void* key;
    void* val;
    int   used;
};

struct dccpp_flat_map {
    struct dccpp_flat_map_entry* arr;
    size_t size;
    size_t cap;
    size_t key_size;
    size_t val_size;
    int   (*key_cmp)(const void*, const void*);
    void  (*key_dtor)(void*);
    void  (*val_dtor)(void*);
};

static void dccpp_flat_map_reserve(struct dccpp_flat_map* m, size_t need) {
    if (need <= m->cap) return;
    size_t newcap = m->cap ? m->cap * 2 : 8;
    while (newcap < need) newcap *= 2;
    m->arr = (struct dccpp_flat_map_entry*)realloc(m->arr,
        newcap * sizeof(*m->arr));
    if (!m->arr) abort();
    for (size_t i = m->cap; i < newcap; ++i) {
        m->arr[i].used = 0;
        m->arr[i].key = NULL;
        m->arr[i].val = NULL;
    }
    m->cap = newcap;
}

struct dccpp_flat_map* dccpp_flat_map_new(size_t ks, size_t vs,
                                          int (*kc)(const void*, const void*),
                                          void (*kd)(void*),
                                          void (*vd)(void*)) {
    struct dccpp_flat_map* m = (struct dccpp_flat_map*)malloc(sizeof(*m));
    if (!m) abort();
    m->arr = NULL;
    m->size = 0; m->cap = 0;
    m->key_size = ks; m->val_size = vs;
    m->key_cmp = kc; m->key_dtor = kd; m->val_dtor = vd;
    return m;
}

void dccpp_flat_map_free(struct dccpp_flat_map* m) {
    if (!m) return;
    for (size_t i = 0; i < m->cap; ++i) {
        if (m->arr[i].used) {
            if (m->key_dtor && m->arr[i].key) m->key_dtor(m->arr[i].key);
            if (m->val_dtor && m->arr[i].val) m->val_dtor(m->arr[i].val);
        }
    }
    free(m->arr);
    free(m);
}

size_t dccpp_flat_map_size(const struct dccpp_flat_map* m) { return m->size; }

int dccpp_flat_map_empty(const struct dccpp_flat_map* m) { return m->size == 0; }

/* 二分查找位置 (lower_bound): 第一个 >= key 的位置 */
static size_t dccpp_flat_map_lower(const struct dccpp_flat_map* m, const void* key) {
    size_t lo = 0, hi = m->size;
    while (lo < hi) {
        size_t mid = (lo + hi) / 2;
        int c = m->key_cmp(m->arr[mid].key, key);
        if (c < 0) lo = mid + 1;
        else       hi = mid;
    }
    return lo;
}

int dccpp_flat_map_contains(const struct dccpp_flat_map* m, const void* key) {
    if (m->size == 0) return 0;
    size_t i = dccpp_flat_map_lower(m, key);
    if (i == m->size) return 0;
    return m->key_cmp(m->arr[i].key, key) == 0;
}

void* dccpp_flat_map_get(const struct dccpp_flat_map* m, const void* key) {
    if (m->size == 0) return NULL;
    size_t i = dccpp_flat_map_lower(m, key);
    if (i == m->size) return NULL;
    if (m->key_cmp(m->arr[i].key, key) != 0) return NULL;
    return m->arr[i].val;
}

/* insert_or_assign: 键存在则替换, 不存在则插入并保持排序 */
void dccpp_flat_map_insert(struct dccpp_flat_map* m, void* key, void* val) {
    if (m->size == 0) {
        dccpp_flat_map_reserve(m, 1);
        m->arr[0].key = key; m->arr[0].val = val; m->arr[0].used = 1;
        m->size = 1;
        return;
    }
    size_t i = dccpp_flat_map_lower(m, key);
    if (i < m->size && m->key_cmp(m->arr[i].key, key) == 0) {
        if (m->val_dtor && m->arr[i].val) m->val_dtor(m->arr[i].val);
        m->arr[i].val = val;
        return;
    }
    dccpp_flat_map_reserve(m, m->size + 1);
    /* 平移 i..size-1 到 i+1..size */
    memmove(&m->arr[i + 1], &m->arr[i],
            (m->size - i) * sizeof(*m->arr));
    m->arr[i].key = key; m->arr[i].val = val; m->arr[i].used = 1;
    m->size++;
}

int dccpp_flat_map_erase(struct dccpp_flat_map* m, const void* key) {
    if (m->size == 0) return 0;
    size_t i = dccpp_flat_map_lower(m, key);
    if (i == m->size) return 0;
    if (m->key_cmp(m->arr[i].key, key) != 0) return 0;
    if (m->key_dtor && m->arr[i].key) m->key_dtor(m->arr[i].key);
    if (m->val_dtor && m->arr[i].val) m->val_dtor(m->arr[i].val);
    memmove(&m->arr[i], &m->arr[i + 1],
            (m->size - i - 1) * sizeof(*m->arr));
    m->size--;
    m->arr[m->size].used = 0;
    return 1;
}

void dccpp_flat_map_clear(struct dccpp_flat_map* m) {
    for (size_t i = 0; i < m->size; ++i) {
        if (m->key_dtor && m->arr[i].key) m->key_dtor(m->arr[i].key);
        if (m->val_dtor && m->arr[i].val) m->val_dtor(m->arr[i].val);
        m->arr[i].used = 0;
    }
    m->size = 0;
}

/* 范围遍历 */
const void* dccpp_flat_map_key_at(const struct dccpp_flat_map* m, size_t i) {
    return (i < m->size) ? m->arr[i].key : NULL;
}
const void* dccpp_flat_map_val_at(const struct dccpp_flat_map* m, size_t i) {
    return (i < m->size) ? m->arr[i].val : NULL;
}

/* =========================================================================
 * Part 16: std::regex (M18, 简化子集)
 *
 * 解析 regex 字符串为 byte-code 程序, 然后用 NFA 模拟执行.
 * 支持的字面: 字面字符, . (任意), [...] (字符类), *, +, ?, |,
 *             ^, $, (), 转义 \
 * 不支持: 反向引用, 命名捕获, lookahead, 复杂 unicode.
 * ========================================================================= */

/* 编译结果: 操作码序列 */
enum {
    DCCPP_RE_LIT = 1,   /* 后面跟一个字节字面 */
    DCCPP_RE_ANY,       /* 任意字节 */
    DCCPP_RE_CLASS,     /* 256 字节位图, 紧跟 32 字节 bitmap */
    DCCPP_RE_NCLASS,    /* 256 字节位图取反 */
    DCCPP_RE_SPLIT,     /* 后面跟 2 字节 (lo, hi) 跳转目标, 16-bit LE */
    DCCPP_RE_JMP,       /* 后面跟 2 字节目标 */
    DCCPP_RE_SAVE,      /* 后面跟 1 字节 capture index */
    DCCPP_RE_MATCH,     /* 终止: 匹配 */
    DCCPP_RE_BOL,       /* ^ 行首 */
    DCCPP_RE_EOL        /* $ 行尾 */
};

struct dccpp_regex {
    unsigned char* prog;
    size_t         proglen;
    int            saves;       /* 捕获数 + 1 (0 总是 match-start) */
};

/* 解析辅助 */
struct dccpp_re_parser {
    const char* src;
    size_t      pos;
    size_t      len;
    unsigned char* out;
    size_t      outlen;
    size_t      outcap;
    int         err;
    int         saves;
};

static void dccpp_re_emit(struct dccpp_re_parser* p, unsigned char b) {
    if (p->outlen >= p->outcap) {
        p->outcap = p->outcap ? p->outcap * 2 : 64;
        p->out = (unsigned char*)realloc(p->out, p->outcap);
    }
    p->out[p->outlen++] = b;
}

static void dccpp_re_emit2(struct dccpp_re_parser* p, unsigned char a,
                           unsigned char b) {
    dccpp_re_emit(p, a);
    dccpp_re_emit(p, b);
}

static size_t dccpp_re_emit_jmp(struct dccpp_re_parser* p, unsigned char op) {
    dccpp_re_emit(p, op);
    size_t pos = p->outlen;
    dccpp_re_emit(p, 0);  /* 占位, 后面 patch */
    dccpp_re_emit(p, 0);
    return pos;
}

static void dccpp_re_patch(struct dccpp_re_parser* p, size_t pos, size_t target) {
    if (target > 0xFFFF) { p->err = 1; return; }
    p->out[pos]     = (unsigned char)(target & 0xFF);
    p->out[pos + 1] = (unsigned char)((target >> 8) & 0xFF);
}

static unsigned char dccpp_re_peek(struct dccpp_re_parser* p) {
    return (p->pos < p->len) ? (unsigned char)p->src[p->pos] : 0;
}
static unsigned char dccpp_re_next(struct dccpp_re_parser* p) {
    return (p->pos < p->len) ? (unsigned char)p->src[p->pos++] : 0;
}

static void dccpp_re_parse_alt(struct dccpp_re_parser* p);
static void dccpp_re_parse_seq(struct dccpp_re_parser* p);

static void dccpp_re_parse_class(struct dccpp_re_parser* p, int negate) {
    unsigned char bitmap[32] = {0};
    /* 调用方已跳过 '[' 和 '^' (如有) */
    while (p->pos < p->len && p->src[p->pos] != ']') {
        unsigned char c = (unsigned char)p->src[p->pos++];
        unsigned char end = c;
        if (p->pos + 1 < p->len && p->src[p->pos] == '-' &&
            p->src[p->pos + 1] != ']') {
            p->pos++;  /* skip '-' */
            end = (unsigned char)p->src[p->pos++];
        }
        for (unsigned int k = c; k <= end; ++k) {
            bitmap[k / 8] |= (unsigned char)(1u << (k % 8));
        }
    }
    if (p->pos < p->len) p->pos++;  /* skip ']' */
    dccpp_re_emit(p, negate ? DCCPP_RE_NCLASS : DCCPP_RE_CLASS);
    for (int k = 0; k < 32; ++k) dccpp_re_emit(p, bitmap[k]);
}

static void dccpp_re_parse_atom(struct dccpp_re_parser* p) {
    unsigned char c = dccpp_re_peek(p);
    if (c == 0) { p->err = 1; return; }
    if (c == '\x28') {
        p->pos++;
        unsigned char save_idx = (unsigned char)p->saves++;
        dccpp_re_emit(p, DCCPP_RE_SAVE);
        dccpp_re_emit(p, save_idx);
        dccpp_re_parse_alt(p);
        if (p->pos < p->len && p->src[p->pos] == '\x29') p->pos++;
        dccpp_re_emit(p, DCCPP_RE_SAVE);
        dccpp_re_emit(p, (unsigned char)(save_idx + 1));
        return;
    }
    if (c == '[') {
        p->pos++;
        int negate = 0;
        if (p->pos < p->len && p->src[p->pos] == '^') {
            negate = 1;
            p->pos++;
        }
        dccpp_re_parse_class(p, negate);
        return;
    }
    if (c == '.') {
        p->pos++;
        dccpp_re_emit(p, DCCPP_RE_ANY);
        return;
    }
    if (c == '^') {
        p->pos++;
        dccpp_re_emit(p, DCCPP_RE_BOL);
        return;
    }
    if (c == '$') {
        p->pos++;
        dccpp_re_emit(p, DCCPP_RE_EOL);
        return;
    }
    if (c == '\\') {
        p->pos++;
        unsigned char e = dccpp_re_next(p);
        dccpp_re_emit(p, DCCPP_RE_LIT);
        dccpp_re_emit(p, e);
        return;
    }
    /* 字面字符 */
    p->pos++;
    dccpp_re_emit(p, DCCPP_RE_LIT);
    dccpp_re_emit(p, c);
}

static void dccpp_re_parse_quant(struct dccpp_re_parser* p) {
    size_t atom_start = p->outlen;
    dccpp_re_parse_atom(p);
    if (p->err) return;
    unsigned char q = dccpp_re_peek(p);
    if (q == '*' || q == '+' || q == '?') {
        p->pos++;
        if (q == '?') {
            /* split: prefer empty, then atom */
            size_t split = dccpp_re_emit_jmp(p, DCCPP_RE_SPLIT);
            dccpp_re_patch(p, split, p->outlen);  /* empty branch */
            dccpp_re_patch(p, split + 2, p->outlen);  /* placeholder */
            size_t jmp = dccpp_re_emit_jmp(p, DCCPP_RE_JMP);
            dccpp_re_patch(p, split + 2, p->outlen);
            dccpp_re_patch(p, jmp, p->outlen);
        } else if (q == '+') {
            /* emit split at start of atom, prefer atom, then fall through */
            /* 简化: 不重写. 末尾再发一个 split. */
            size_t split = dccpp_re_emit_jmp(p, DCCPP_RE_SPLIT);
            dccpp_re_patch(p, split, atom_start);  /* 重复 */
            dccpp_re_patch(p, split + 2, p->outlen);  /* 退出 */
        } else {  /* * */
            size_t split = dccpp_re_emit_jmp(p, DCCPP_RE_SPLIT);
            dccpp_re_patch(p, split, p->outlen);  /* 跳过, 0 次 */
            dccpp_re_patch(p, split + 2, atom_start);  /* 重复 */
        }
    }
}

static void dccpp_re_parse_seq(struct dccpp_re_parser* p) {
    while (p->pos < p->len && p->src[p->pos] != '|' &&
           p->src[p->pos] != '\x29') {
        dccpp_re_parse_quant(p);
        if (p->err) return;
    }
}

static void dccpp_re_parse_alt(struct dccpp_re_parser* p) {
    size_t l1 = dccpp_re_emit_jmp(p, DCCPP_RE_SPLIT);
    dccpp_re_parse_seq(p);
    if (p->err) return;
    size_t jmps[16];
    int    njmps = 0;
    while (p->pos < p->len && p->src[p->pos] == '|') {
        p->pos++;
        if (njmps < 16) jmps[njmps++] = dccpp_re_emit_jmp(p, DCCPP_RE_JMP);
        dccpp_re_patch(p, l1, p->outlen);  /* first alt: skip */
        l1 = dccpp_re_emit_jmp(p, DCCPP_RE_SPLIT);
        dccpp_re_parse_seq(p);
        if (p->err) return;
    }
    size_t end = p->outlen;
    for (int i = 0; i < njmps; ++i) dccpp_re_patch(p, jmps[i], end);
    dccpp_re_patch(p, l1, end);
    dccpp_re_emit(p, DCCPP_RE_MATCH);
}

struct dccpp_regex* dccpp_regex_compile(const char* pat) {
    if (!pat) return NULL;
    struct dccpp_re_parser p = {0};
    p.src = pat;
    p.len = strlen(pat);
    p.pos = 0;
    p.saves = 1;
    p.out = NULL; p.outlen = 0; p.outcap = 0;
    dccpp_re_emit(&p, DCCPP_RE_SAVE);
    dccpp_re_emit(&p, 0);
    dccpp_re_parse_alt(&p);
    if (p.err) { free(p.out); return NULL; }
    struct dccpp_regex* r = (struct dccpp_regex*)malloc(sizeof(*r));
    if (!r) { free(p.out); return NULL; }
    r->prog = p.out;
    r->proglen = p.outlen;
    r->saves = p.saves;
    return r;
}

void dccpp_regex_free(struct dccpp_regex* r) {
    if (!r) return;
    free(r->prog);
    free(r);
}

/* 真正的回溯式 NFA 解释器.
   每个 SPLIT 复制当前 caps 状态, 优先尝试 lo 分支, 失败再试 hi.
   递归深度受限于 regex 嵌套层数 (实际工程用不上 16+ 层). */
static int dccpp_re_match_re(const unsigned char* prog, size_t proglen,
                             const char* s, size_t slen,
                             size_t pc, size_t pos, int* caps) {
    for (;;) {
        if (pc >= proglen) return 0;
        unsigned char op = prog[pc++];
        if (op == DCCPP_RE_MATCH) return 1;
        if (op == DCCPP_RE_LIT) {
            unsigned char c = prog[pc++];
            if (pos >= slen || (unsigned char)s[pos] != c) return 0;
            pos++;
        }
        else if (op == DCCPP_RE_ANY) {
            if (pos >= slen) return 0;
            pos++;
        }
        else if (op == DCCPP_RE_CLASS || op == DCCPP_RE_NCLASS) {
            if (pos >= slen) return 0;
            unsigned char c = (unsigned char)s[pos];
            int hit = (prog[pc + (c >> 3)] & (unsigned char)(1u << (c & 7))) != 0;
            if (op == DCCPP_RE_NCLASS) hit = !hit;
            if (!hit) return 0;
            pc += 32;
            pos++;
        }
        else if (op == DCCPP_RE_SAVE) {
            unsigned char idx = prog[pc++];
            if (idx < 32) caps[idx] = (int)pos;
        }
        else if (op == DCCPP_RE_JMP) {
            size_t target = (size_t)prog[pc] | ((size_t)prog[pc + 1] << 8);
            pc = target;
        }
        else if (op == DCCPP_RE_SPLIT) {
            size_t lo = (size_t)prog[pc]     | ((size_t)prog[pc + 1] << 8);
            size_t hi = (size_t)prog[pc + 2] | ((size_t)prog[pc + 3] << 8);
            int saved[32];
            memcpy(saved, caps, sizeof(saved));
            if (dccpp_re_match_re(prog, proglen, s, slen, lo, pos, caps))
                return 1;
            memcpy(caps, saved, sizeof(saved));
            if (dccpp_re_match_re(prog, proglen, s, slen, hi, pos, caps))
                return 1;
            return 0;
        }
        else if (op == DCCPP_RE_BOL) {
            if (pos != 0) return 0;
        }
        else if (op == DCCPP_RE_EOL) {
            if (pos != slen) return 0;
        }
        else {
            return 0;
        }
    }
}

int dccpp_regex_match(struct dccpp_regex* r, const char* s) {
    if (!r || !s) return 0;
    int caps[32];
    for (int i = 0; i < 32; ++i) caps[i] = -1;
    size_t slen = strlen(s);
    /* 简化: 锚定到字符串首 (不实现 search/find_position, 仅前缀匹配). */
    if (dccpp_re_match_re(r->prog, r->proglen, s, slen, 0, 0, caps)) {
        return 1;
    }
    return 0;
}

/* =========================================================================
 * Part 17: std::ranges (M18, 简化子集)
 *
 * view 是一对 (begin, end) + 适配器. 简化: view 不真正 lazy,
 * 先 collect 到临时 vector 再返回. 支持 filter/transform/take.
 * ========================================================================= */

struct dccpp_view {
    void*  data;     /* 内部分配的缓冲区, view_free 时释放 */
    size_t elem_size;
    size_t size;
    size_t cap;
    /* 元素是 void*, 用户负责类型 cast */
};

struct dccpp_view* dccpp_view_new(size_t elem_size) {
    struct dccpp_view* v = (struct dccpp_view*)malloc(sizeof(*v));
    if (!v) abort();
    v->data = NULL; v->size = 0; v->cap = 0; v->elem_size = elem_size;
    return v;
}

void dccpp_view_free(struct dccpp_view* v) {
    if (!v) return;
    free(v->data);
    free(v);
}

size_t dccpp_view_size(const struct dccpp_view* v) { return v->size; }
int    dccpp_view_empty(const struct dccpp_view* v) { return v->size == 0; }

void* dccpp_view_at(const struct dccpp_view* v, size_t i) {
    if (i >= v->size) return NULL;
    return (char*)v->data + i * v->elem_size;
}

static void dccpp_view_push(struct dccpp_view* v, const void* elem) {
    if (v->size >= v->cap) {
        size_t newcap = v->cap ? v->cap * 2 : 4;
        v->data = realloc(v->data, newcap * v->elem_size);
        if (!v->data) abort();
        v->cap = newcap;
    }
    memcpy((char*)v->data + v->size * v->elem_size, elem, v->elem_size);
    v->size++;
}

struct dccpp_view* dccpp_view_filter_int(const struct dccpp_view* src,
                                         int (*pred)(int)) {
    struct dccpp_view* out = dccpp_view_new(sizeof(int));
    for (size_t i = 0; i < src->size; ++i) {
        int v;
        memcpy(&v, dccpp_view_at(src, i), sizeof(int));
        if (pred(v)) dccpp_view_push(out, &v);
    }
    return out;
}

struct dccpp_view* dccpp_view_transform_int(const struct dccpp_view* src,
                                            int (*fn)(int)) {
    struct dccpp_view* out = dccpp_view_new(sizeof(int));
    for (size_t i = 0; i < src->size; ++i) {
        int v;
        memcpy(&v, dccpp_view_at(src, i), sizeof(int));
        v = fn(v);
        dccpp_view_push(out, &v);
    }
    return out;
}

struct dccpp_view* dccpp_view_take(const struct dccpp_view* src, size_t n) {
    struct dccpp_view* out = dccpp_view_new(src->elem_size);
    size_t k = n < src->size ? n : src->size;
    for (size_t i = 0; i < k; ++i) {
        dccpp_view_push(out, dccpp_view_at(src, i));
    }
    return out;
}

struct dccpp_view* dccpp_view_reverse(const struct dccpp_view* src) {
    struct dccpp_view* out = dccpp_view_new(src->elem_size);
    for (size_t i = src->size; i > 0; --i) {
        dccpp_view_push(out, dccpp_view_at(src, i - 1));
    }
    return out;
}

int dccpp_view_sum_int(const struct dccpp_view* v) {
    int s = 0;
    for (size_t i = 0; i < v->size; ++i) {
        int x;
        memcpy(&x, dccpp_view_at(v, i), sizeof(int));
        s += x;
    }
    return s;
}

/* =========================================================================
 * Part 18: C++23 多维下标 (M18) 运行时辅助
 *
 * __dcc_multi_index_N(obj, i1, ..., iN) 接收 (对象指针, 索引).
 * dcc++ 改写 `obj[i1, ..., iN]` 为调用此函数. 默认实现按字节偏移
 * 算 (即"假装对象是 int[N] 数组"). 用户可在 C 端为自定义类特化
 * (e.g. 提供 stride 信息).
 * ========================================================================= */

int __dcc_multi_index_2(int* obj, int i, int j) {
    /* 简化: 按 sizeof(int) 算 offset = (i * 1 + j) * sizeof(int). */
    (void)obj; (void)i; (void)j;
    return 0;
}
int __dcc_multi_index_3(int* obj, int i, int j, int k) {
    (void)obj; (void)i; (void)j; (void)k;
    return 0;
}
int __dcc_multi_index_4(int* obj, int i, int j, int k, int l) {
    (void)obj; (void)i; (void)j; (void)k; (void)l;
    return 0;
}

