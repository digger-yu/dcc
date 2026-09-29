/* asm_fallback_gas.c - 混合"已知/未知"助记符的 TU, 验证 --asm-fallback 的两条出口。
 *
 * 判据面:
 *   - `serialize` / `wbnoinvd` 是 GAS 认得、dcc 指令表没有的**真实** x86_64 指令
 *     (Linux arch/x86 用于 hypervisor 与内存序屏障), 因此这一条 asm 语句整体
 *     必须被记为"透传给 GAS", 而不是被静默编码或 NOP 掉;
 *   - `nop` 是 dcc 能编码的, 与它同处一个 TU —— 这就是方案文档要求的
 *     "混合已知指令和未知指令, 验证整个 TU 正确降级"的边界夹具;
 *   - 默认 (--asm-fallback 未给) 必须报 E0311 硬错误, 保持向后兼容。
 */

int barrier_mix(void)
{
	__asm__ volatile("nop");
	/* 一条语句里两行未知指令: 诊断必须按语句聚合, 不得刷出两条重复错误 */
	__asm__ volatile("serialize\n\twbnoinvd\n\t");
	return 0;
}
