/* C89 Trigraphs test */
/* Trigraphs are enabled with -trigraphs flag */

??=include <stdio.h>

int main(void) ??<
    /* ??( -> [ and ??) -> ] */
    int arr??(3??);
    arr??(0??) = 1;
    arr??(1??) = 2;
    arr??(2??) = 3;

    /* ??< -> { and ??> -> } */
    struct Point ??<
        int x;
        int y;
    ??> p;
    p.x = 10;
    p.y = 20;

    /* ??/ -> \ (line continuation) */
    int x = 1 + ??/
        2;

    /* ??' -> ^ */
    int xor_val = 5 ??' 3;

    /* ??! -> | */
    int or_val = 4 ??! 2;

    /* ??- -> ~ */
    int not_val = ??-0;

    /* Trigraphs in string literals should NOT be converted */
    const char *s = "??= ??( ??/";

    printf("arr: %d %d %d\n", arr??(0??), arr??(1??), arr??(2??));
    printf("point: %d %d\n", p.x, p.y);
    printf("x: %d\n", x);
    printf("xor: %d\n", xor_val);
    printf("or: %d\n", or_val);
    printf("not: %d\n", not_val);
    printf("str: %s\n", s);

    return 0;
??>
