/* Test fixture: shldw/shrdw 16-bit double precision shift */

unsigned short test_shldw_imm(unsigned short dst, unsigned short src)
{
    unsigned short result = dst;
    __asm__ __volatile__(
        "shldw %2, %1, %0"
        : "+r"(result)
        : "r"(src), "i"(4)
    );
    return result;
}

unsigned short test_shrdw_imm(unsigned short dst, unsigned short src)
{
    unsigned short result = dst;
    __asm__ __volatile__(
        "shrdw %2, %1, %0"
        : "+r"(result)
        : "r"(src), "i"(3)
    );
    return result;
}

unsigned short test_shldw_cl(unsigned short dst, unsigned short src)
{
    unsigned short result = dst;
    __asm__ __volatile__(
        "shldw %%cl, %1, %0"
        : "+r"(result)
        : "r"(src), "c"(5)
    );
    return result;
}

unsigned short test_shrdw_cl(unsigned short dst, unsigned short src)
{
    unsigned short result = dst;
    __asm__ __volatile__(
        "shrdw %%cl, %1, %0"
        : "+r"(result)
        : "r"(src), "c"(2)
    );
    return result;
}
