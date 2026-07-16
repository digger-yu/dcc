void test(void)
{
	asm __inline volatile ("nop");
	asm __inline volatile ("x" : : );
	asm __inline volatile ("x" : [p] "=a" (p) : "i" (0));
}
