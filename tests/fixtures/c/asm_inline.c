void test(void)
{
	asm __inline volatile ("nop");
	asm __inline volatile ("nop" : : );
	asm __inline volatile ("nop" : [p] "=a" (p) : [q] "i" (0));
}
