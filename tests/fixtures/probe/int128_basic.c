/* __int128 探针: 乘除模都需要 (非原生架构应落到成对寄存器或 libcall) */
__int128 i128_mul(__int128 a, __int128 b) { return a * b; }
__int128 i128_div(__int128 a, __int128 b) { return a / b; }
__int128 i128_mod(__int128 a, __int128 b) { return a % b; }
