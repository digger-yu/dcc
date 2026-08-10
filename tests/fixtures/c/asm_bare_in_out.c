/* bare in/out (no suffix) + memory-indirect port forms */

static unsigned short g_io_port;

void test_bare_in_out(void) {
	/* bare in/out with immediate port (default 32-bit)
	 * AT&T: in port, dest / out src, port */
	asm volatile("in $0x80, %eax");
	asm volatile("out %eax, $0x80");
	/* bare in/out with DX port */
	asm volatile("in %dx, %eax");
	asm volatile("out %eax, %dx");
}

void test_mem_indirect_port(void) {
	/* memory-indirect port: port loaded from memory
	 * in port_mem, dest / out src, port_mem */
	asm volatile("in %1, %0" :: "a"(0), "m"(g_io_port));
	asm volatile("out %0, %1" :: "a"(0), "m"(g_io_port));
	/* suffixed in/out with memory-indirect port */
	asm volatile("inb %1, %0" :: "a"(0), "m"(g_io_port));
	asm volatile("outb %0, %1" :: "a"(0), "m"(g_io_port));
}

int main(void) {
	g_io_port = 0x80;
	test_bare_in_out();
	test_mem_indirect_port();
	return 0;
}
