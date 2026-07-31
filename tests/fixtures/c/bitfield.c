typedef unsigned short u16;

struct desc_struct {
	u16 limit0;
	u16 base0;
	u16 base1: 8, type: 4, s: 1, dpl: 2, p: 1;
	u16 limit1: 4, avl: 1, l: 1, d: 1, g: 1, base2: 8;
} __attribute__((packed));

int use(void) { return sizeof(struct desc_struct); }
