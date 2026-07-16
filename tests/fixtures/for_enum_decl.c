enum mod_mem_type { MOD_INIT_TEXT, MOD_MEM_NUM_TYPES };

static inline int test_fn(void)
{
	for (enum mod_mem_type (type) = 0; (type) < MOD_MEM_NUM_TYPES; (type)++)
		if ((type) == MOD_INIT_TEXT)
			return 1;
	return 0;
}

int main(void) { return test_fn(); }
