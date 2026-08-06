// C23 #elifdef / #elifndef 条件指令端到端 (M26)
#define FEATURE_A 1
int main(void) {
    int r = 0;
#if 0
    r += 1;
#elifdef FEATURE_A
    r += 2;
#else
    r += 3;
#endif
#if 1
    r += 4;
#elifndef NOT_DEFINED
    r += 8;
#else
    r += 16;
#endif
#if 0
    r += 32;
#elifndef ANOTHER_MISSING
    r += 64;
#endif
#if 0
    r += 128;
#elifdef FEATURE_A
    r += 256;
#elifndef YET_ANOTHER
    r += 512;
#else
    r += 1024;
#endif
    // r = 2 + 4 + 64 + 256 = 326
    if (r != 326) return 1;
    return 0;
}
