/* 下划线开头的**用户标识符**不得被当成编译器合成临时值。
 *
 * SSA→AST 重建为了少发栈槽, 会把「合成临时值」的单次使用内联到使用点、并跳过它的
 * 赋值语句。旧实现用「base 以 `_` 开头」判定合成值 —— C 里 `_s` / `__d` / `_tbin`
 * 都是合法的用户标识符, 于是它们的赋值被内联掉、循环推进语句直接消失:
 *   `int _s=0; for(i<n) _s+=i;` 在 -O2 起返回错值; 分块 pass 自己生成的块索引
 *   `__dcc_tile_i0` 也被吞掉推进 ⇒ -O3 忙等。
 * 反向也一样错: 本仓 pass 生成的 `dcc_t<序号>_xxx` 不以 `_` 开头。
 *
 * 判据现在是 `ssa::is_synthetic_temp_base` —— 只信「用户可见存储」的单一真源
 * (`original_locals ∪ params`), 与 `Builder::is_local_object` 同源。
 *
 * 通过: exit 0（期望值取自 gcc -O0 参照）
 */

/* 下划线名做循环携带累加器（推进赋值不可被内联掉） */
int acc_underscore(int n) {
    int _s = 0;
    for (int i = 0; i < n; i++) {
        _s += i;
    }
    return _s;
}

/* 双下划线前缀 + while 携带 */
int while_underscore(int n) {
    int __t = 0;
    while (n > 0) {
        __t += n;
        n--;
    }
    return __t;
}

/* 名字正好撞上内部临时值的命名风格（`_t` 前缀），必须是用户变量 */
int collision_shaped(int n) {
    int _tbin = 3;
    int s = 0;
    for (int i = 0; i < n; i++) {
        s += _tbin * i;
        _tbin = _tbin + 1;
    }
    return s + _tbin;
}

/* `_` 名字被内联进使用点后仍要保序：体内读写同一个下划线变量 */
int order_sensitive(int n) {
    int _x = 1;
    int s = 0;
    for (int i = 0; i < n; i++) {
        int _y = _x;
        s += _y * i;
        _x = _x + 2;
    }
    return s;
}

/* `_` 名字做指针别名目标：与「被取址局部」两条判据叠加 */
int taken_underscore(void) {
    int _acc = 0;
    int *p = &_acc;
    for (int i = 1; i <= 3; i++) {
        *p = *p + i;
    }
    return _acc;
}

/* 参数名下划线 + 取址 */
int param_underscore(int _n) {
    int *p = &_n;
    _n = _n + 1;
    return _n + *p;
}

int main(void) {
    if (acc_underscore(4) != 6)
        return 1;
    if (acc_underscore(0) != 0)
        return 2;
    if (while_underscore(4) != 10)
        return 3;
    if (collision_shaped(4) != 39)
        return 4;
    if (order_sensitive(3) != 13)
        return 5;
    if (taken_underscore() != 6)
        return 6;
    if (param_underscore(4) != 10)
        return 7;
    return 0;
}
