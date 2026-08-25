/* kernel_code_model_data.c: Verify data symbol relocations in -mcmodel=kernel
 *
 * In kernel code model, ALL symbol references (data and function) must
 * bypass GOT indirect addressing. Data references use RIP-relative
 * addressing with R_X86_64_PC32 relocations; function references use
 * R_X86_64_PLT32. Neither uses R_X86_64_GOTPCREL.
 *
 * This fixture is compiled with -mcmodel=kernel and the integration test
 * checks readelf output to confirm no GOTPCREL relocations are emitted.
 */

int k_global_var = 42;
extern int k_ext_var;
static int k_static_var = 7;

int k_read_global(void) { return k_global_var; }
int k_read_ext(void) { return k_ext_var; }
int k_read_static(void) { return k_static_var; }

int *k_addr_global(void) { return &k_global_var; }
int *k_addr_ext(void) { return &k_ext_var; }
int *k_addr_static(void) { return &k_static_var; }

void k_write_global(int v) { k_global_var = v; }
void k_write_ext(int v) { k_ext_var = v; }

/* Function address taking should also bypass GOT */
int k_internal_func(void) { return 1; }
extern int k_ext_func(void);

int (*k_get_internal(void))(void) { return k_internal_func; }
int (*k_get_ext(void))(void) { return k_ext_func; }
