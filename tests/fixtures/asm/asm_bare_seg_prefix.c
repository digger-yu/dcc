/* Test fixture: bare segment prefix as standalone instruction */

void test_ds_prefix(void)
{
    /* Old-style assembly: ds as standalone instruction */
    __asm__ __volatile__(
        "ds\n\t"
        "nop"
    );
}

void test_es_prefix(void)
{
    __asm__ __volatile__(
        "es\n\t"
        "nop"
    );
}

void test_cs_prefix(void)
{
    __asm__ __volatile__(
        "cs\n\t"
        "nop"
    );
}

void test_ss_prefix(void)
{
    __asm__ __volatile__(
        "ss\n\t"
        "nop"
    );
}

void test_fs_prefix(void)
{
    __asm__ __volatile__(
        "fs\n\t"
        "nop"
    );
}

void test_gs_prefix(void)
{
    __asm__ __volatile__(
        "gs\n\t"
        "nop"
    );
}
