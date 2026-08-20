/* lib/baz.c - kernel object corpus sample */
#include <linux/string.h>

int lib_baz(const char *s)
{
	return s ? 1 : 0;
}