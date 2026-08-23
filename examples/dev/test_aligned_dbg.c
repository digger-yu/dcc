#include <stdio.h>
struct A { int x; } __attribute__((__aligned__(16)));
int main() { printf("size=%zu align=%zu\n", sizeof(struct A), __alignof__(struct A)); return 0; }
