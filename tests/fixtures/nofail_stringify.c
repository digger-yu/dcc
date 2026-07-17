#include <stdlib.h>

#define NOFAIL(ptr) do_nofail((ptr), #ptr)

void *do_nofail(void *ptr, const char *expr);

void *test_nofail(void) {
	return NOFAIL(malloc(sizeof(int) +
			    1));
}

int main(void) { (void)test_nofail(); return 0; }
