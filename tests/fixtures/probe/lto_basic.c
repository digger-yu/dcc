/* LTO 探针: 跨函数调用才可能被写进 LLVM/DCC bitcode 段 */
int lto_leaf(int x) { return x + 3; }
int lto_root(int y) { return lto_leaf(y) * lto_leaf(y ^ 7); }
