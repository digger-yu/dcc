/* Daily hotspot: indirect calls through function pointers.
 *
 * Pins the call *%reg instruction (FF /2) and the register chosen
 * for the closure target. Also covers the call-preserved vs
 * call-clobbered discipline at the call site.
 */
typedef int (*Fn)(int);

int call_with(Fn f, int x) {
    return f(x);
}

int caller_calls(int arg) {
    static Fn table[] = {
        0,  /* f1 */
    };
    /* indirect call through a register */
    extern int identity(int);
    Fn fp = &identity;
    return fp(arg);
}
