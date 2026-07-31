#define __KERNEL_DIV_ROUND_UP(n, d) (((n) + (d) - 1) / (d))
#define DIV_ROUND_UP __KERNEL_DIV_ROUND_UP
#define XA_CHUNK_SIZE (1UL << (0 ? 4 : 6))
#define BITS_PER_LONG 64
#define XA_MARK_LONGS DIV_ROUND_UP(XA_CHUNK_SIZE, BITS_PER_LONG)

unsigned long tags[3][XA_MARK_LONGS];

int main(void) { return 0; }
