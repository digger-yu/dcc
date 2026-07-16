void __attribute__((__section__(".init.text"))) __attribute__((__indirect_branch__("keep"))) *early_memremap_encrypted(unsigned long phys_addr, unsigned long size);

int main(void) { return 0; }
