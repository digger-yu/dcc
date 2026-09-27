/*
 * switch 下跳必须按 **case 值** 直查入口块, 不得从块名前缀反推。
 *
 * 根因 (src/cpp/lower_ir.rs): 建块时块名按 `case_<值>` 约定生成, 下跳阶段却用
 * `ssa.blocks.keys().find(|k| k.starts_with("case_<值>"))` 反查。`blocks` 是
 * HashMap (迭代序随进程随机播种), 匹配又是 **前缀** 匹配, 于是 `case_1` 会
 * 命中 `case_11`: 含 ≥10 个 case 的函数每次编译可能把不同的 case 接到同一
 * 入口 —— 无诊断、随机失效的静默错编译。
 *
 * 因此本夹具故意把「短值」与「以它为前缀的长值」放在同一个 switch 里, 并且
 * 每个 case 交付一个独一无二的字符串, 任何一条边接错都会立刻反映在 stdout 与
 * 退出码上; 外层/内层 switch 用同一批数值, 检验嵌套 switch 各自持表、内层不
 * 误命中外层标签。
 *
 * 退出码 = 错编译的 case 条数 (0 = 全部与 g++ 一致)。
 */
#include <cstdio>
#include <cstring>

/* 一个 case 边的验收: 字符串不符则计 1 条错编译 */
static int chk_str(const char *got, const char *want) {
    printf("got=%s want=%s\n", got, want);
    return strcmp(got, want) != 0 ? 1 : 0;
}

/* 前缀碰撞密集区: 1/11/12/111/121, 2/22/222, 3/33 */
static const char *dense(int i) {
    switch (i) {
        case 1:   return "c1";
        case 11:  return "c11";
        case 111: return "c111";
        case 12:  return "c12";
        case 121: return "c121";
        case 2:   return "c2";
        case 22:  return "c22";
        case 222: return "c222";
        case 3:   return "c3";
        case 33:  return "c33";
        case 0:   return "c0";
        case 10:  return "c10";
        case 13:  return "c13";
        default:  return "cd";
    }
}

/* 14 个顺序 case: 触发 `case_1` vs `case_1x` 前缀歧形的最小自然形态 */
static int conv(int i) {
    switch (i) {
        case 0: return 100;
        case 1: return 101;
        case 2: return 102;
        case 3: return 103;
        case 4: return 104;
        case 5: return 105;
        case 6: return 106;
        case 7: return 107;
        case 8: return 108;
        case 9: return 109;
        case 10: return 110;
        case 11: return 111;
        case 12: return 112;
        case 13: return 113;
        default: return 199;
    }
}

/* 嵌套 switch: 内外层共用同一批 case 值, 两张表不得互相误命中 */
static int nested(int a, int b) {
    int acc = 0;
    switch (a) {
        case 1:
            acc += 10;
            switch (b) {
                case 1:  acc += 1;
                case 11: acc += 11;
                case 2:  acc += 2;
                default: acc += 1000;
            }
            break;
        case 11:
            acc += 110;
            switch (b) {
                case 1: acc += 3; break;
                case 2: acc += 4; break;
            }
            break;
        default:
            acc += 7;
    }
    return acc;
}

int main() {
    int bad = 0;

    /* 前缀碰撞密集区: 每个值单独交付一串, 接错边则字符串对不上 */
    bad += chk_str(dense(1), "c1");
    bad += chk_str(dense(11), "c11");
    bad += chk_str(dense(111), "c111");
    bad += chk_str(dense(12), "c12");
    bad += chk_str(dense(121), "c121");
    bad += chk_str(dense(2), "c2");
    bad += chk_str(dense(22), "c22");
    bad += chk_str(dense(222), "c222");
    bad += chk_str(dense(3), "c3");
    bad += chk_str(dense(33), "c33");
    bad += chk_str(dense(0), "c0");
    bad += chk_str(dense(10), "c10");
    bad += chk_str(dense(13), "c13");
    bad += chk_str(dense(99), "cd");

    for (int i = 0; i <= 13; i++) {
        int got = conv(i);
        printf("conv %d -> %d\n", i, got);
        if (got != 100 + i) bad++;
    }
    if (conv(14) != 199) bad++;

    /* 逐条列出期望值, 前缀错接会改变 fallthrough 后的累加结果 */
    printf("nested %d %d %d %d\n", nested(1, 1), nested(1, 11), nested(1, 2), nested(11, 1));
    if (nested(1, 1) != 10 + 1 + 11 + 2 + 1000) bad++;
    if (nested(1, 11) != 10 + 11 + 2 + 1000) bad++;
    if (nested(1, 2) != 10 + 2 + 1000) bad++;
    if (nested(11, 1) != 110 + 3) bad++;
    if (nested(11, 2) != 110 + 4) bad++;
    if (nested(22, 1) != 7) bad++;

    printf("bad=%d\n", bad);
    return bad;
}
