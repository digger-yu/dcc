// 补齐 GCC 预定义宏 (M19)
// 验证 __amd64__/__amd64/__x86_64/__k8__/__k8/__x86_64__/__USER_LABEL_PREFIX__/
// __VERSION__/__OPTIMIZE__/__NO_INLINE__/__SEG_GS/__SEG_FS 全部已预定义
int main(void) {
#if !defined(__amd64__) || !defined(__amd64) || !defined(__x86_64) \
 || !defined(__k8__) || !defined(__k8) || !defined(__x86_64__)
    return 1;
#endif
#if !defined(__USER_LABEL_PREFIX__) || !defined(__VERSION__) \
 || !defined(__OPTIMIZE__) || !defined(__NO_INLINE__) \
 || !defined(__SEG_GS) || !defined(__SEG_FS)
    return 2;
#endif
    return 0;
}
