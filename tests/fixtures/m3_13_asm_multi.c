void test(void)
{
	asm volatile ("cmp %1,%2; sbb %0,%0;"
			:"=r" (0)
			:"g"(1),"r" (2)
			:"cc");
}
