/* LICM 禁止夹具 ④：体内含函数调用与指针间接写。
 *
 * 调用可能被改写的一切"可观测对象"（全局 ∪ volatile ∪ static/_Thread_local 局部 ∪
 * 地址逃逸的局部）都必须逐个记进名字栅栏，否则 `int t = counter;` 会被当成不变式，
 * 4 次采样被压成 1 次（要求 1/3）。
 */
int counter = 0;

int bump(void) {
    counter = counter + 3;
    return counter;
}

/* 体内调用 ⇒ counter 归入可观测对象，读它的声明不可外提。 */
int gather(int n) {
    int s = 0;
    while (n > 0) {
        int t = counter;
        s += t;
        counter = bump();
        n--;
    }
    return s;
}

/* 没有调用，但 acc 被取址、且体内**经指针**改写 ⇒ 间接写同样要 clobber。
   若把 `int t = acc;` 外提，s 只累加首轮采到的 0，结果从 14 退化为 6。 */
int indirect(int n) {
    int acc = 0;
    int *p = &acc;
    int s = 0;
    while (n > 0) {
        int t = acc;
        s += t;
        *p = *p + n;
        n--;
    }
    return s + *p;
}

int main(void) {
    if (gather(3) != 9) return 1;
    if (counter != 9) return 2;
    if (gather(0) != 0) return 3;
    if (counter != 9) return 4;
    if (indirect(3) != 14) return 5;
    if (indirect(0) != 0) return 6;
    return 0;
}
