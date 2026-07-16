struct desc_struct {
	unsigned a;
	unsigned s;
	unsigned dpl;
	unsigned p;
};

static inline void set_desc(struct desc_struct *d)
{
	*d = (struct desc_struct){ .s = 1, .dpl = 0, .p = 1 };
	asm inline("nop");
}
