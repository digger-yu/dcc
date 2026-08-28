// examples/dev/deep_anon_struct.c
// Test 3+ levels of anonymous struct/union nesting
// (kernel struct_group() macro expansion pattern)

// 3-level nesting: struct -> union -> struct
struct level3 {
    union {
        struct {
            int x;
            int y;
        };
        struct {
            long z;
        };
    };
    int w;
};

// 4-level nesting: struct -> struct -> union -> struct
struct level4 {
    struct {
        union {
            struct {
                int a;
                int b;
            };
            float c;
        };
    };
    int d;
};

// Kernel struct_group() pattern
#define struct_group(GRP, MEMBERS...) \
    union { struct { MEMBERS }; struct { MEMBERS } GRP; }

struct with_group {
    int before;
    struct_group(group,
        int g1;
        int g2;
    );
    int after;
};

// Anonymous struct with __attribute__ at deep level
struct with_attrs {
    union {
        struct {
            int val;
        } __attribute__((packed));
        long raw;
    };
};

int test_access(void) {
    struct level3 a;
    a.x = 1;    // transparent through union -> struct
    a.y = 2;
    a.z = 3;    // transparent through union -> struct (alt)
    a.w = 4;

    struct level4 b;
    b.a = 10;   // transparent through struct -> union -> struct
    b.b = 20;
    b.d = 30;

    struct with_group c;
    c.before = 1;
    c.g1 = 2;   // transparent through anonymous union -> struct
    c.after = 3;

    struct with_attrs d;
    d.val = 42;
    d.raw = 99;

    return a.x + b.a + c.g1 + d.val;
}
