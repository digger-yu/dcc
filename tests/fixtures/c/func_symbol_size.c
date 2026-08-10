/* Test fixture: function symbol size correctness */

/* Weak function - should have non-zero size */
int __attribute__((weak)) weak_func(void)
{
    return 42;
}

/* Static function - should have non-zero size */
static int static_func(int x)
{
    return x * 2 + 1;
}

/* Alias function - should inherit target's size */
int real_func(void)
{
    return 100;
}
int alias_func(void) __attribute__((alias("real_func")));

/* Normal function called by alias */
int caller(void)
{
    return weak_func() + static_func(3) + alias_func();
}
