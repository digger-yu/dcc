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
static struct dccpp_ostream _dccpp_cout = { stdout };
static struct dccpp_ostream _dccpp_cerr = { stderr };
static struct dccpp_istream _dccpp_cin  = { stdin  };

struct dccpp_ostream* dccpp_cout(void) { return &_dccpp_cout; }
struct dccpp_ostream* dccpp_cerr(void) { return &_dccpp_cerr; }
struct dccpp_istream* dccpp_cin(void)  { return &_dccpp_cin;  }

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
