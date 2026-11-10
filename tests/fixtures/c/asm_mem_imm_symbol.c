/* §5.28: `op $imm, sym(%rip)` 的重定位加数必须按「disp32 后面还有立即数」算。
 *
 * 旧实现里 `pending_data_rels` 的 PC32 加数恒为 -4，而 CPU 取 RIP 的位置是
 * **指令尾**：`addl $9, g(%rip)` 后面还跟着 imm8，基准就后移 1 字节 ⇒ 写到了
 * g+1（读回 0x900 而不是 9）；imm32 形状（0x81 /0 id）偏 4 字节。
 * 同一个发射臂还漏了宽度分派：8 位必须走 `80 /digit ib`，发 0x83/0x81 会把
 * 单字节加变成 DWORD 写（邻接字节被清掉，静态变量之间的 padding 会掩盖它）。
 *
 * 失败码：1=add imm8、2=add imm32、3=sub imm8、4=addq、5=shl、
 *         6=sar、7=addb 符号、8=addb 数组元素（邻接哨兵）、9=imul imm,sym,reg
 */
static int g;
static long gl;
static unsigned char gb;
static unsigned char trio[3] = {0xAA, 0x11, 0xBB};

int main(void)
{
    asm volatile("addl $9, %0" : "+m"(g));
    if (g != 9) return 1;

    asm volatile("addl $0x1234, %0" : "+m"(g));
    if (g != 0x1234 + 9) return 2;

    asm volatile("subl $7, %0" : "+m"(g));
    if (g != 0x1234 + 2) return 3;

    asm volatile("addq $9, %0" : "+m"(gl));
    if (gl != 9) return 4;

    asm volatile("shll $4, %0" : "+m"(g));
    if (g != (0x1234 + 2) << 4) return 5;

    asm volatile("sarl $1, %0" : "+m"(g));
    if (g != ((0x1234 + 2) << 4) >> 1) return 6;

    asm volatile("addb $3, %0" : "+m"(gb));
    if (gb != 3) return 7;

    asm volatile("addb $3, %0" : "+m"(trio[1]));
    if (trio[0] != 0xAA || trio[1] != 0x14 || trio[2] != 0xBB) return 8;

    int r;
    asm volatile("imul $5, %1, %0" : "=r"(r) : "m"(g));
    if (r != g * 5) return 9;

    return 0;
}
