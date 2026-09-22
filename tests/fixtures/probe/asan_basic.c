/* ASan 探针: 4 字节 load 必须留下 __asan_load4 这类 UND 引用 */
int ld4(int *p) { return *p; }
