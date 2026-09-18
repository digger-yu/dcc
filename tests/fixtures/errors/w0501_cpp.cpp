/* W0501: C++ [[fallthrough]] 隐式贯穿诊断 (C++ native backend)
 *
 * C++ 端必须与 C 端行为一致:
 * - implicit_fallthrough: 未标注贯穿 -> W0501, 定位到 case 标签
 * - marked_fallthrough:   [[fallthrough]] 标注 -> 不警告
 * - gnu_marked_fallthrough: __attribute__((fallthrough)) 标注 -> 不警告
 * - return_terminated:    return 终止 -> 不警告
 * - empty_case:           空 case 体 -> 不警告
 */
int implicit_fallthrough(int x) {
    int r = 0;
    switch (x) {
    case 1:
        r += 10;
    case 2:
        r += 20;
        break;
    default:
        r += 30;
    }
    return r;
}

int marked_fallthrough(int x) {
    int r = 0;
    switch (x) {
    case 1:
        r += 10;
        [[fallthrough]];
    case 2:
        r += 20;
        break;
    default:
        break;
    }
    return r;
}

int gnu_marked_fallthrough(int x) {
    int r = 0;
    switch (x) {
    case 1:
        r += 10;
        __attribute__((fallthrough));
    case 2:
        r += 20;
        break;
    default:
        break;
    }
    return r;
}

int return_terminated(int x) {
    switch (x) {
    case 1:
        return 10;
    case 2:
        return 20;
    default:
        return 30;
    }
}

int empty_case(int x) {
    int r = 0;
    switch (x) {
    case 1:
    case 2:
        r += 20;
        break;
    default:
        break;
    }
    return r;
}

int misplaced_fallthrough(int x) {
    int r = 0;
    switch (x) {
    case 1:
        [[fallthrough]];
        r += 10;
    case 2:
        r += 20;
        break;
    default:
        break;
    }
    return r;
}
