/* arch/x86/kernel/foo.c - kernel object corpus sample */

int arch_x86_kernel_foo(void)
{
	return 42;
}

int arch_x86_kernel_bar(int x)
{
	return x + arch_x86_kernel_foo();
}