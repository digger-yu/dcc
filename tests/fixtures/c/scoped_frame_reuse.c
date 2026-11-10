/* §5.26 帧槽复用（frame economy）的判据语料。
 *
 * 这不是「跑出结果」的夹具，而是让**扁平水位分配**与**块作用域分配**的帧大小
 * 拉开数量级的形状：
 *   - frame_subject 里有 12 个互不重叠的块，每块 6 个 **唯一名字** 的 volatile
 *     局部（volatile 保证它们真的占帧槽，不会被折叠成寄存器值；唯一名字保证
 *     它们不会被「同名共享一格」的既有规则合并掉）；
 *   - mix() 被内联 4 次，每次一层内联块 + 6 个（由内联器改名的）局部。
 *
 * 旧实现把函数体摊平后单调累加水位 ⇒ 上述 ~96 个槽全部相加；
 * 作用域感知实现下，兄弟块/已退出的内联块共用同一批槽（≈12~18 槽）。
 *
 * 红法：让 `pre_register_vars` 退回「块退出不回收水位」⇒ frame_subject 的
 * `sub $N,%rsp` 立刻涨回 ~0x300+，`block_scopes_reuse_frame_slots` 变红。
 *
 * 语义侧同时与 gcc 四档对拍：槽复用只能发生在生命周期不相交的变量之间，
 * 任何一次「活变量被别人的值盖掉」都会在这里被读出来。
 */

static int mix(int x)
{
    int a = x + 1;
    int b = a * 3;
    int c = b - 7;
    int d = c ^ 0x5a;
    int e = d + a;
    int f = e * 2 - b;
    return a + b + c + d + e + f;
}

int frame_subject(int n)
{
    int acc = 0;
    for (int i = 0; i < n; i++) {
        {
            volatile int v00 = i + 0, v01 = v00 + 1, v02 = v01 + 1;
            volatile int v03 = v02 + 1, v04 = v03 + 1, v05 = v04 + 1;
            acc += v00 + v01 + v02 + v03 + v04 + v05;
        }
        {
            volatile int v10 = i + 2, v11 = v10 + 1, v12 = v11 + 1;
            volatile int v13 = v12 + 1, v14 = v13 + 1, v15 = v14 + 1;
            acc += v10 + v11 + v12 + v13 + v14 + v15;
        }
        {
            volatile int v20 = i + 4, v21 = v20 + 1, v22 = v21 + 1;
            volatile int v23 = v22 + 1, v24 = v23 + 1, v25 = v24 + 1;
            acc += v20 + v21 + v22 + v23 + v24 + v25;
        }
        {
            volatile int v30 = i + 6, v31 = v30 + 1, v32 = v31 + 1;
            volatile int v33 = v32 + 1, v34 = v33 + 1, v35 = v34 + 1;
            acc += v30 + v31 + v32 + v33 + v34 + v35;
        }
        {
            volatile int v40 = i + 8, v41 = v40 + 1, v42 = v41 + 1;
            volatile int v43 = v42 + 1, v44 = v43 + 1, v45 = v44 + 1;
            acc += v40 + v41 + v42 + v43 + v44 + v45;
        }
        {
            volatile int v50 = i + 10, v51 = v50 + 1, v52 = v51 + 1;
            volatile int v53 = v52 + 1, v54 = v53 + 1, v55 = v54 + 1;
            acc += v50 + v51 + v52 + v53 + v54 + v55;
        }
        {
            volatile int v60 = i + 12, v61 = v60 + 1, v62 = v61 + 1;
            volatile int v63 = v62 + 1, v64 = v63 + 1, v65 = v64 + 1;
            acc += v60 + v61 + v62 + v63 + v64 + v65;
        }
        {
            volatile int v70 = i + 14, v71 = v70 + 1, v72 = v71 + 1;
            volatile int v73 = v72 + 1, v74 = v73 + 1, v75 = v74 + 1;
            acc += v70 + v71 + v72 + v73 + v74 + v75;
        }
        {
            volatile int v80 = i + 16, v81 = v80 + 1, v82 = v81 + 1;
            volatile int v83 = v82 + 1, v84 = v83 + 1, v85 = v84 + 1;
            acc += v80 + v81 + v82 + v83 + v84 + v85;
        }
        {
            volatile int v90 = i + 18, v91 = v90 + 1, v92 = v91 + 1;
            volatile int v93 = v92 + 1, v94 = v93 + 1, v95 = v94 + 1;
            acc += v90 + v91 + v92 + v93 + v94 + v95;
        }
        {
            volatile int va0 = i + 20, va1 = va0 + 1, va2 = va1 + 1;
            volatile int va3 = va2 + 1, va4 = va3 + 1, va5 = va4 + 1;
            acc += va0 + va1 + va2 + va3 + va4 + va5;
        }
        {
            volatile int vb0 = i + 22, vb1 = vb0 + 1, vb2 = vb1 + 1;
            volatile int vb3 = vb2 + 1, vb4 = vb3 + 1, vb5 = vb4 + 1;
            acc += vb0 + vb1 + vb2 + vb3 + vb4 + vb5;
        }
        acc += mix(i);
        acc += mix(i + 1);
        acc += mix(i + 2);
        acc += mix(i + 3);
    }
    return acc;
}

int main(void)
{
    /* 对拍约定：main 必须返回 0 才表示「与参照一致」——
     * `run_gcc_cross_check_opt` 先断言两侧退出码相等，再要求它等于 0。
     * n=5 的结果 10814 由 gcc -O0/-O1/-O2/-O3 四档实测一致；
     * 任何一次「活变量被已退出块的变量盖掉」都会改变这个值，
     * 于是这里返回 1，两侧退出码一起变红。 */
    return frame_subject(5) == 10814 ? 0 : 1;
}
