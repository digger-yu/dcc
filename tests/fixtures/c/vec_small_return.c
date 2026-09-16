typedef unsigned char v1qu __attribute__((vector_size(1)));
typedef unsigned short v1hu __attribute__((vector_size(2)));
typedef unsigned int v1ui __attribute__((vector_size(4)));
typedef unsigned short v2hu __attribute__((vector_size(4)));
typedef unsigned char v4qu __attribute__((vector_size(4)));

v1qu ret_v1qu(void) { return (v1qu){0x7fu}; }
v1hu ret_v1hu(void) { return (v1hu){0x1122u}; }
v1ui ret_v1ui(void) { return (v1ui){0x11223344u}; }
v2hu ret_v2hu(void) { return (v2hu){0x1122u, 0x3344u}; }
v4qu ret_v4qu(void) { return (v4qu){0x11u, 0x22u, 0x33u, 0x44u}; }

int main(void) {
    v1qu x = ret_v1qu();
    v1hu y = ret_v1hu();
    v1ui a = ret_v1ui();
    v2hu b = ret_v2hu();
    v4qu c = ret_v4qu();
    if (x[0] != 0x7fu) return 1;
    if (y[0] != 0x1122u) return 2;
    if (a[0] != 0x11223344u) return 3;
    if (b[0] != 0x1122u || b[1] != 0x3344u) return 4;
    if (c[0] != 0x11u || c[1] != 0x22u || c[2] != 0x33u || c[3] != 0x44u) return 5;
    return 0;
}
