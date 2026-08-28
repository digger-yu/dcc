#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define DCC_PGO_SITE_COUNT 2u
#define DCC_PGO_MAX_VALUE_SITES 2u
#define DCC_PGO_MAX_VALUE_TARGETS 16u

static const char *_dcc_pgo_source = "/root/dcc/tests/fixtures/c/profile.c";
static const char *_dcc_pgo_funcs[DCC_PGO_SITE_COUNT] = { "square", "main" };
static const char *_dcc_pgo_files[DCC_PGO_SITE_COUNT] = { "/root/dcc/tests/fixtures/c/profile.c", "/root/dcc/tests/fixtures/c/profile.c" };
static const uint32_t _dcc_pgo_lines[DCC_PGO_SITE_COUNT] = { 2, 3 };
static uint64_t _dcc_pgo_counters[DCC_PGO_SITE_COUNT];
static int _dcc_pgo_initialized;
static const char *_dcc_pgo_out_path = "/tmp/dcc-pgo-gen-test/profile.profraw";

struct _dcc_pgo_value_target {
    const char *name;
    uint64_t count;
};

struct _dcc_pgo_value_site {
    uint32_t in_use;
    uint32_t n_targets;
    struct _dcc_pgo_value_target targets[DCC_PGO_MAX_VALUE_TARGETS];
};

static struct _dcc_pgo_value_site _dcc_pgo_values[DCC_PGO_MAX_VALUE_SITES];

static void _dcc_pgo_write_profraw(void);

static void __dcc_pgo_init(void) {
    if (_dcc_pgo_initialized) return;
    _dcc_pgo_initialized = 1;
    atexit(_dcc_pgo_write_profraw);
}

static uint64_t _dcc_pgo_fnv1a_bytes(uint64_t h, const unsigned char *p,
                                      size_t n) {
    for (size_t i = 0; i < n; i++) {
        h ^= (uint64_t)p[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

static uint64_t _dcc_pgo_checksum(void) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (uint32_t i = 0; i < DCC_PGO_SITE_COUNT; i++) {
        h = _dcc_pgo_fnv1a_bytes(h, (const unsigned char *)_dcc_pgo_funcs[i],
                                 strlen(_dcc_pgo_funcs[i]));
        h = _dcc_pgo_fnv1a_bytes(h, (const unsigned char *)_dcc_pgo_files[i],
                                 strlen(_dcc_pgo_files[i]));
        h ^= (uint64_t)_dcc_pgo_lines[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

static void _dcc_pgo_wr32(FILE *f, uint32_t v) {
    unsigned char b[4] = {
        (unsigned char)(v & 0xff),
        (unsigned char)((v >> 8) & 0xff),
        (unsigned char)((v >> 16) & 0xff),
        (unsigned char)((v >> 24) & 0xff),
    };
    fwrite(b, 1, 4, f);
}

static void _dcc_pgo_wr64(FILE *f, uint64_t v) {
    unsigned char b[8];
    for (int i = 0; i < 8; i++) b[i] = (unsigned char)((v >> (8 * i)) & 0xff);
    fwrite(b, 1, 8, f);
}

static void _dcc_pgo_wrstr(FILE *f, const char *s) {
    uint32_t n = (uint32_t)strlen(s);
    _dcc_pgo_wr32(f, n);
    fwrite(s, 1, n, f);
}

void __dcc_pgo_count_fn(uint32_t idx) {
    __dcc_pgo_init();
    if (idx < DCC_PGO_SITE_COUNT) _dcc_pgo_counters[idx]++;
}

void __dcc_pgo_value_sample(uint32_t idx) {
    __dcc_pgo_init();
    if (idx >= DCC_PGO_MAX_VALUE_SITES) return;
    struct _dcc_pgo_value_site *site = &_dcc_pgo_values[idx];
    site->in_use = 1;
    const char *target = _dcc_pgo_funcs[idx];
    for (uint32_t i = 0; i < site->n_targets; i++) {
        if (strcmp(site->targets[i].name, target) == 0) {
            site->targets[i].count++;
            return;
        }
    }
    if (site->n_targets < DCC_PGO_MAX_VALUE_TARGETS) {
        site->targets[site->n_targets].name = target;
        site->targets[site->n_targets].count = 1;
        site->n_targets++;
    }
}

static void _dcc_pgo_write_profraw(void) {
    if (!_dcc_pgo_initialized) return;
    FILE *f = fopen(_dcc_pgo_out_path, "wb");
    if (!f) return;
    fwrite("PROF", 1, 4, f);
    _dcc_pgo_wr32(f, 2u);
    _dcc_pgo_wr64(f, _dcc_pgo_checksum());
    _dcc_pgo_wr32(f, DCC_PGO_SITE_COUNT);
    for (uint32_t i = 0; i < DCC_PGO_SITE_COUNT; i++) {
        _dcc_pgo_wrstr(f, _dcc_pgo_funcs[i]);
        _dcc_pgo_wrstr(f, _dcc_pgo_files[i]);
        _dcc_pgo_wr32(f, _dcc_pgo_lines[i]);
    }
    uint32_t n_vp = 0;
    for (uint32_t i = 0; i < DCC_PGO_MAX_VALUE_SITES; i++) {
        if (_dcc_pgo_values[i].in_use && _dcc_pgo_values[i].n_targets) n_vp++;
    }
    _dcc_pgo_wr32(f, n_vp);
    for (uint32_t i = 0; i < DCC_PGO_MAX_VALUE_SITES; i++) {
        if (!_dcc_pgo_values[i].in_use || !_dcc_pgo_values[i].n_targets) continue;
        _dcc_pgo_wr32(f, i);
        _dcc_pgo_wr32(f, _dcc_pgo_values[i].n_targets);
        for (uint32_t j = 0; j < _dcc_pgo_values[i].n_targets; j++) {
            _dcc_pgo_wrstr(f, _dcc_pgo_values[i].targets[j].name);
            _dcc_pgo_wr64(f, _dcc_pgo_values[i].targets[j].count);
        }
    }
    _dcc_pgo_wr32(f, DCC_PGO_SITE_COUNT);
    for (uint32_t i = 0; i < DCC_PGO_SITE_COUNT; i++) {
        _dcc_pgo_wr64(f, _dcc_pgo_counters[i]);
    }
    fclose(f);
    _dcc_pgo_initialized = 0;
}
