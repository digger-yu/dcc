// 补齐 GCC 预定义宏 (M19)
// 验证 __amd64__/__amd64/__x86_64/__k8__/__k8/__x86_64__/__USER_LABEL_PREFIX__/
// __VERSION__/__SEG_GS/__SEG_FS 全部已预定义。
//
// __OPTIMIZE__ 与 __NO_INLINE__ 必须**恰好定义一个**：gcc 只在启用优化时定义前者，
// 只在不做内联的 -O0 下定义后者。本夹具因此只断言「互斥且必有一个」这一件与档位
// 无关的事实 —— 直接 #if !defined(__OPTIMIZE__) 会让 -O0 与 -O2 拿到不同退出码，
// 那是对预定义宏的正确行为提要求，而不是优化器改变了行为（`check_opt_equivalence.sh`
// 的三档一致不变式会把这种夹具报成 OPT-DIFF）。
int main(void) {
#if !defined(__amd64__) || !defined(__amd64) || !defined(__x86_64) \
 || !defined(__k8__) || !defined(__k8) || !defined(__x86_64__)
    return 1;
#endif
#if !defined(__USER_LABEL_PREFIX__) || !defined(__VERSION__) \
 || !defined(__SEG_GS) || !defined(__SEG_FS)
    return 2;
#endif
#if !defined(__OPTIMIZE__) && !defined(__NO_INLINE__)
    return 3; /* 两个都没定义: 档位信息丢了 */
#endif
#if defined(__OPTIMIZE__) && defined(__NO_INLINE__)
    return 4; /* 同时定义: 与 gcc 不符 */
#endif
    return 0;
}
