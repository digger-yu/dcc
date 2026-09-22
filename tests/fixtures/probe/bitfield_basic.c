/* 位域读写探针。刻意不含 sizeof/offsetof —— 那些是 SizeAlignOffset 的坑, 与位域无关 */
struct bf { unsigned int a : 3; unsigned int b : 5; unsigned int c : 24; };
unsigned int bf_read(struct bf s) { return s.a + s.b * 2 + s.c; }
struct bf bf_write(unsigned int v) { struct bf s; s.a = v & 7; s.b = (v >> 3) & 31; s.c = v >> 8; return s; }
