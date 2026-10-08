// src/codegen/x86_64/abi/regbank.rs
//
// 寄存器占用的**唯一真源**。
//
// 为什么要有这个对象：`docs/p12-physreg-spill-design.md` §3 的 H4 —— 在引入本类型之前，
// 「谁占着哪个物理寄存器」这件事在后端有四套各自维护的判据：
//   1. `Codegen::reg_bindings`  ——  名字 → (号, 声明类型)，只回答「谁住在这儿」；
//   2. `Codegen::reg_allocated`  ——  u8 位图，表达式溅出池的发号依据；
//   3. `Codegen::callee_saved_used` —— u8 位图，序言/尾声保存的回填依据；
//   4. `AsmRegisterPlanner::used_gp/clobber_gp` —— 内联汇编规划器自己的一份集合。
// `docs/ssa-pass-upgrade-plan.md` §7.15 的实测结论可以直接引用：**所有权判据一旦有两套，
// 弱的那一套决定产物**。那三条存量错值（调用者的 %rbx 被毁、变量的家被临时值盖掉、
// file-scope 绑定整目标文件 0 次保存）全都是「某套判据没看到另一套的事实」。
//
// 本类型把 2/3 的**含义**收进来（两个位图现在是这里的私有字段），并把 4 的初始集合
// 由 `owned_gp()` 一次性给出。1（名字→号）仍然留在 `reg_bindings`：那是「谁住在这儿」
// 的映射，不是「哪个号不能再发出去」的占用判断；但每一次绑定都必须 `claim()` 进来，
// 于是「绑定的号一定被钉住」由类型而不是由调用点的自觉维持。
//
// 分 bank：§6 的 Q3 决定 Location 的号按 bank 解释（GP / XMM / x87），本类型先只对
// GP 的 callee-saved 池建立占用模型 —— 那是今天自动分配唯一能碰的四个号
// （`rbx/r12/r14/r15`，`r13` 是帧基址）。caller-saved 与 XMM/x87 的占用模型是
// Phase 5 的事（设计 §6 D4：必须排在干涉表之后），`pool_for_bank` 对它们返回空集。

use std::collections::HashSet;

/// 寄存器编号的**通道**。编号域是分 bank 的（§6 Q3），而这件事的定义只有一份：
/// 它住在中立类型层（`crate::location::RegBank`），因为「一个值的家在哪条通道」既要被
/// 分配器写、也要被后端解释，分两处定义就一定会漂移。本后端只做一个本地别名。
///
/// 为什么这里没有 MMX/段/控制寄存器：`Location` 表达的是「一个值能住在哪」，
/// 而这三类在 dcc 里从来不是家（绑定类型今天只允许平台标准整型与指针，`bindable_ty`）。
/// 它们属于指令操作数的寄存器类，留在 `codegen::asm_event::RegClass`，两边不混用。
pub type Bank = crate::location::RegBank;

/// 自动分配可用的 callee-saved GP 池。**下标即位序**：`bit i` 归属 `POOL[i]`。
/// 这个顺序同时是序言/尾声四条保存的顺序（见 `emit/func_setup.rs::emit_func_epilogue`
/// 的 `callee_save_patch_sites` 与 bit 0x01/0x02/0x04/0x08），换序就是换产物。
const POOL: [u8; 4] = [3, 12, 14, 15]; // rbx, r12, r14, r15

/// 表达式求值的**落点寄存器**（`docs/p12-physreg-spill-design.md` §7 Phase 5.1 前置一）。
///
/// 这是「结果恒在 %rax」那条发射约定的**唯一出处**。以前它以内联字面量 `0` 的形式
/// 抄在了 252 个调用点上（实测 `grep -rE 'gen_expr\([^)]*, 0\)'`），而抄本不能回答
/// 任何一个 P12 必须回答的问题：一个值现在到底在哪个寄存器里？基址要改就得同时改
/// 完所有抄本（而这正是「谁也不知道自己改全了没有」的形状，与本仓 H4 同一类）。
///
/// 名字而不是一行注释：`Self::EVAL_BASE`（`Codegen` 上的别名）就是调用点要写的东西。
/// 与 SysV 返回值寄存器、与本后端的名字表是否一致，由下面的用例钉住（不是口头约定）。
pub const EVAL_BASE_REG: u8 = 0; // %rax

/// 二元运算**右操作数的暂存号**候选表（Phase 5.1 前置三）。
///
/// 以前这个号在 `emit/expr/dispatch.rs` 里写死 `1`（连同三处 rcx 专属字节编码），意思就是
/// 「`%rcx` 永远归暂存」—— 这条约定凌驾在占用事实上，所以把 caller-saved 号发给一个变量
/// 当家以后，紧跟其后的求值会把那个家踩掉（实测：`a+b+c+d+e` 里 `c` 的家给 `%rcx` ⇒
/// gcc 50 / dcc 47，设计 §11.40）。
///
/// **候选表不在这里定义**：它就是分配器的延伸池（`src/ssa/regalloc.rs::AUTO_EXT_POOL`）。
/// 以前两边各自列了一份同样的 5 元表，而「哪些号能暂存」与「哪些号能当家」其实是同一个
/// 事实的两面：家把候选占满后，发射期才发现无号可暂存（实测 `E1135`，§11.47）。现在单源
/// 在分配器那一侧，它同时负责「发号时必须给暂存留一个空闲候选」这条判据。
///
/// 排除项（`rax` 求值基址、`rdx` 128 位返回高半、`r10` 静态链/非局部跳转、`r11` 帧寻址
/// scratch、`rsp/rbp/r13` 固定用途）也同一源。
pub fn rhs_stage_candidates() -> Vec<u8> {
    crate::ssa::regalloc::auto_ext_pool_regs()
}

impl RegBank {
    /// 给一次二元运算选右操作数的暂存号：跳过 `dst`（左右同开会直接把操作数吃掉）与
    /// 任何已被握着的号（家：`allocated`；用户/自动钉住：`pinned`）。
    ///
    /// 返 `None` 意思是候选表全被握着 —— 调用方不得因此拿一个握着的号，而是去拿
    /// 一个真正空闲的临时号（`alloc_temp`，它会同时把 `used_by_self` 置上，序言保存自动跟上）。
    /// 分配器已保证这种情况不会发生（`stage_free_after`），所以调用方碰到它应当硬错误。
    pub fn pick_rhs_stage(&self, dst: u8) -> Option<u8> {
        let cands = rhs_stage_candidates();
        let got = cands
            .iter()
            .copied()
            .find(|&n| n != dst && !self.is_allocated(n) && !self.is_pinned(n));
        if std::env::var_os("DCC_STAGE_PROBE").is_some() {
            eprintln!(
                "STAGE dst={} alloc={:08b} pinned={:?} -> {:?}",
                dst,
                self.allocated_mask(),
                {
                    let mut v: Vec<u8> = rhs_stage_candidates()
                        .into_iter()
                        .filter(|&n| self.is_pinned(n))
                        .collect();
                    v.sort();
                    v
                },
                got
            );
        }
        got
    }
}

/// 这个 GP 号是不是**调用会被毁掉**的那一类（SysV AMD64）。
///
/// 为什么需要它作为一个可问的事实（Phase 5.1 前置三的第一块）：本后端到今天为止只把
/// 「callee-saved 池」（`POOL`）当一份名单，而「跨 call 保活」需要的恰好是它的补集：
/// 分配器选家、文件作用域 `register asm` 绑定、内联 asm 的 clobber 表，问的都是同一个问题
/// （「这个号能不能存下一个值」），以前各自在心里背了一份。
///
/// 不叫 `is_callee_saved` 而叫 `is_call_clobbered`：两个集合并不是互补的。SysV 里
/// `%rsp`/`%rbp` 是固定用途寄存器（前者本后端当只读别名允许绑定），它们既不「被调用毁」
/// 也不是可供分配使用的 callee-saved 家；`%r13` 是本后端的帧基址。这三个号归「不毁」，
/// 能不能拿当家是另一道门（`reserved_reg_reason`）负责的。
///
/// 实测对拍 gcc（`register unsigned long v asm("<r>");` 文件作用域）：`rax/rcx/rdx/rsi/rdi/
/// r8-r11` 全部报 `call-clobbered register used for global register variable`，
/// `rbx/r12/r13/r14/r15/rsp` 不报（`rbp` 是一条 error，不属本判据）。
pub fn is_call_clobbered(n: u8) -> bool {
    // SysV AMD64：callee-saved = rbx(3)、rbp(5)、r12-r15(12..15)；rsp(4) 固定；
    // 剩下的（rax 0、rcx 1、rdx 2、rsi 6、rdi 7、r8-r11 = 8..11）都是 caller-saved。
    // APX 的 r16-r31 不在本表的已知号里（绑定与 clobber 侧另有名字表门），这里一律当毁。
    !matches!(n, 3 | 4 | 5 | 12 | 13 | 14 | 15)
}

/// 一个函数的寄存器占用状态。随 `compile_func` 逐函数复位（`reset_reg_tracker`）。
#[derive(Debug, Clone)]
pub struct RegBank {
    /// 可分配的 callee-saved GP 池（`POOL[i]` ↔ `bit i`）。
    pool: [u8; 4],
    /// `bit i`：`pool[i]` 已经有了「家」—— 要么是某个绑定变量的家，要么是外层表达式
    /// 正握着的溅出槽。发号器 (`alloc_temp`) 必须跳过这些位。
    allocated: u8,
    /// `bit i`：本函数使用了 `pool[i]`，因此序言/尾声对应的保存/恢复必须保留而不得被
    /// `patch_callee_saves` 改成 nop。只置这一位是不够的：`allocated` 才是「别人不能
    /// 再拿走」的依据，两者由 `claim`/`alloc_temp` 成对维护。
    used_by_self: u8,
    /// 被某个 `register asm` 变量钉住的号（**不限** callee-saved 池：用户写
    /// `asm("r8")` 时号也在钉着，只是它不在自动分配的池里、`allocated` 无位可置）。
    /// `release` 拒绝释放钉住的号，`owned_gp` 把它并进汇编规划器的排除集合。
    pinned: HashSet<u8>,
    /// `bit i`：`pool[i]` 被本函数某条 **`asm goto`** 列进 clobber，而那条 asm 可以把控制
    /// 流带出「保存/恢复」守护的区间（P12 Phase 5.2a）。这种号此后不得再当临时值发出：
    /// 守护的一对 `push`/`pop` 只覆盖 fall-through 路径，跳走的那条路上没有 `pop`。
    /// 只在 `reset` 时回退（逐函数复位），因为「本函数里出现过这样一条 asm」是函数级事实。
banned: u8,
}

impl Default for RegBank {
    fn default() -> Self {
        Self::new()
    }
}

impl RegBank {
    pub fn new() -> Self {
        Self {
            pool: POOL,
            allocated: 0,
            used_by_self: 0,
            pinned: HashSet::new(),
            banned: 0,
        }
    }

    // ---- 位序 ↔ 号 ----------------------------------------------------------

    /// 号在池里的下标；不在池里（caller-saved、`rsp`/`rbp`/`r13`、APX 的 r16-r31）
    /// 返回 `None`。位图只有四位，池外的号**没有位可置**，这是今天能力边界的真形体。
    pub fn index_of(&self, reg: u8) -> Option<usize> {
        (0..self.pool.len()).find(|&i| self.pool[i] == reg)
    }

    /// 号对应的位掩码；池外为 `None`。
    pub fn bit_of(&self, reg: u8) -> Option<u8> {
        self.index_of(reg).map(|i| 1u8 << i)
    }

    pub fn pool(&self) -> &[u8; 4] {
        &self.pool
    }

    // ---- 占用操作 -----------------------------------------------------------

    /// 绑定即占用：把号钉住，并在它属于池时同时置 `allocated` 与 `used_by_self`。
    ///
    /// 返回是否命中池（命中 ⇒ 两个位都置过）。命中的两条理由各对应一条实测错值，
    /// 见 `docs/ssa-pass-upgrade-plan.md` §7.15：漏 `used_by_self` ⇒ 本函数毁掉调用者
    /// 存在该寄存器里的值（序言那条保存被 patch 成 nop）；漏 `allocated` ⇒ 表达式
    /// 溅出池把同一个号当临时值再发一次，直接盖掉变量的家。
    pub fn claim(&mut self, reg: u8) -> bool {
        self.pinned.insert(reg);
        match self.bit_of(reg) {
            Some(bit) => {
                self.allocated |= bit;
                self.used_by_self |= bit;
                true
            },
            None => false,
        }
    }

    /// 临时值发号：取池里第一个还没被占用、也没被禁止的号，并登记「本函数用了它」。
    /// 顺序必须是 `pool` 下标升序 —— 表达式溅出槽的实际选择会进产物。
    pub fn alloc_temp(&mut self) -> Option<u8> {
        for i in 0..self.pool.len() {
            let bit = 1u8 << i;
            if self.allocated & bit == 0 && self.banned & bit == 0 {
                self.allocated |= bit;
                self.used_by_self |= bit;
                return Some(self.pool[i]);
            }
        }
        None // 池已耗尽：调用方退回帧槽
    }

    /// 宣告「这个号此后不得当临时值发出」（见 `banned` 字段的注释）。
    ///
    /// 不动 `used_by_self`：被禁止的号如果本函数已经发过保存，那条保存仍然要留着
    /// （调用者的值还得还回去）；如果没发过，它也不会有新的占用点。
    /// 命中池返回 `true`；池外的号本来就不在发号范围内，返回 `false` 而不报错 ——
    /// `asm goto` clobber 一个 caller-saved 号是完全正常的写法，本函数不得因此改变行为。
    pub fn ban(&mut self, reg: u8) -> bool {
        match self.bit_of(reg) {
            Some(bit) => {
                self.banned |= bit;
                true
            },
            None => false,
        }
    }

    /// 此刻池里**正持有值**的号（变量的家与表达式溅出槽都在内：`claim` 与 `alloc_temp`
    /// 都置 `allocated`）。内联 asm 的守护集合就是这份读数与 clobber 名单的交集。
    pub fn held_pool_regs(&self) -> Vec<u8> {
        (0..self.pool.len())
            .filter(|&i| self.allocated & (1u8 << i) != 0)
            .map(|i| self.pool[i])
            .collect()
    }

    /// 归还临时值：只清 `allocated`。
    ///
    /// **不清 `used_by_self`** —— 这是旧 `free_callee_saved` 的行为，也必须保持：序言一
    /// 旦为某个寄存器发过保存，就不能因为「后来溅出槽归还了」而把它改成 nop，否则
    /// 变量/调用者值的恢复会整条消失（发射时序见 §7 的 H3：序言先于函数体）。
    ///
    /// 被钉住的号（变量的家）不允许归还。旧实现是到 `reg_bindings` 里现查，判据等价
    /// （每个绑定点都紧跟一次 `claim`，函数入口 `reset` 后按绑定表重新 claim），但
    /// 那条判断本都属于占用问题，故收在这里。
    pub fn release(&mut self, reg: u8) -> bool {
        if self.pinned.contains(&reg) {
            return false;
        }
        if let Some(bit) = self.bit_of(reg) {
            self.allocated &= !bit;
        }
        true
    }

    /// 逐函数复位：清空占用与钉住集合，池定义不动。
    /// 调用方复位后必须立刻按当前绑定表逐个 `claim`（`reset_reg_tracker` 就是这条），
    /// 否则 file-scope 绑定「先填表、后清零」的顺序会让序言少一条保存。
    pub fn reset(&mut self) {
        self.allocated = 0;
        self.used_by_self = 0;
        self.banned = 0;
        self.pinned.clear();
    }

    /// **只**重建钉住集合，不动两个位图。
    ///
    /// 为什么需要这个窄入口（独立复核拓出的可达窗口）：`reg_bindings` 在每次
    /// `compile_func` 入口就 `clear()` + 重填，而 `pinned` 原本只在后面的
    /// `reset_reg_tracker()` 里清；`naked` 分支在 `reset_reg_tracker()` **之前**就 `return`，
    /// 于是两者脱钩。`owned_gp()` 两个方向都会漂：多排除（上一个函数的池外绑定还钉着，
    /// 改变本句 asm 的发号）与少排除（本函数刚填的 file-scope 绑定没进 `pinned`，
    /// asm 操作数可能被分到变量住的那个号 —— 静默错值）。
    ///
    /// 不能用 `claim` 补：它顺带置 `allocated`/`used_by_self`，而旧代码在 naked 路径上
    /// 恰恰**不**做这件事（它靠现查 `reg_bindings` 只拿到名单）。绑定表是「谁住在这儿」
    /// 的真源，`pinned` 必须是它的投影，而不是另一份需要人记得同步的副本。
    pub fn resync_pins<I: IntoIterator<Item = u8>>(&mut self, regs: I) {
        self.pinned.clear();
        self.pinned.extend(regs);
    }

    // ---- 查询 ---------------------------------------------------------------

    /// 溅出池的占用位图（旧 `Codegen::reg_allocated`）。
    pub fn allocated_mask(&self) -> u8 {
        self.allocated
    }

    /// 序言/尾声的保存位图（旧 `Codegen::callee_saved_used`）。
    pub fn used_mask(&self) -> u8 {
        self.used_by_self
    }

    pub fn is_allocated(&self, reg: u8) -> bool {
        matches!(self.bit_of(reg), Some(bit) if self.allocated & bit != 0)
    }

    /// 该号是否被某个变量钉住（与是否在池里无关）。
    pub fn is_pinned(&self, reg: u8) -> bool {
        self.pinned.contains(&reg)
    }

    pub fn is_free(&self, reg: u8) -> bool {
        !self.is_allocated(reg) && !self.is_pinned(reg) && !self.is_banned(reg)
    }

    /// 该号是否被「会把控制流带出守护区间的 asm」禁止当临时值用。
    pub fn is_banned(&self, reg: u8) -> bool {
        matches!(self.bit_of(reg), Some(bit) if self.banned & bit != 0)
    }

    /// 池里当前可发的号（下标升序，排除已占用与被禁止的）。
    pub fn free_temps(&self) -> Vec<u8> {
        self.pool
            .iter()
            .copied()
            .filter(|&r| !self.is_allocated(r) && !self.is_banned(r))
            .collect()
    }

    /// 内联汇编规划器的排除集合：一次取全，取代旧的「转达」调用点。
    ///
    /// 含钉住的号（含池外的用户绑定）与溅出池正占着的号。这些号进规划器的
    /// `used_gp` 而**不进** `clobber_gp`：它们不是被本条 asm 毁掉的，asm 之后原占用者
    /// 还要继续用（§7.17 立的那条规矩，混起来会让 clobber 名单变假）。
    pub fn owned_gp(&self) -> Vec<u8> {
        let mut out: Vec<u8> = self.pinned.iter().copied().collect();
        for i in 0..self.pool.len() {
            if self.allocated & (1u8 << i) != 0 {
                out.push(self.pool[i]);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    // ---- 分 bank 接口 -------------------------------------------------------

    /// 该 bank 里「自动分配可动」的号池。GP 只有 callee-saved 四个；其余通道为空 ——
    /// 空集的含义是「本类型尚未建模」，调用方不得据此推断「这个通道没有寄存器」。
    pub fn pool_for_bank(bank: Bank) -> &'static [u8] {
        match bank {
            Bank::Gp => &POOL,
            Bank::Simd | Bank::FloatStack => &[],
        }
    }

    /// 号属于哪个 bank。今天后端只有 GP 通道参与占用建模，故一律答 `Gp`；
    /// 待 Phase 5 引入 XMM/x87 的家时，本函数是分派点（也是 `Location::Reg`
    /// 编号域按 bank 解释的落点，见设计 §6 的 Q3）。
    pub fn bank_of(_reg: u8) -> Bank {
        Bank::Gp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 位序必须是池下标序，且与序言四条保存的 patch 位一致。
    /// 这条是「换序就是换产物」的钉子：bit0=%rbx, bit1=%r12, bit2=%r14, bit3=%r15。
    #[test]
    fn pool_index_is_bit_order_and_matches_prologue_patch_bits() {
        let bank = RegBank::new();
        assert_eq!(bank.index_of(3), Some(0));
        assert_eq!(bank.index_of(12), Some(1));
        assert_eq!(bank.index_of(14), Some(2));
        assert_eq!(bank.index_of(15), Some(3));
        assert_eq!(bank.bit_of(3), Some(0x01));
        assert_eq!(bank.bit_of(12), Some(0x02));
        assert_eq!(bank.bit_of(14), Some(0x04));
        assert_eq!(bank.bit_of(15), Some(0x08));
        // 池外：caller-saved、固定用途、APX 扩展号一律没有位。
        for reg in [0u8, 1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 13, 16, 31] {
            assert_eq!(bank.index_of(reg), None, "号 {} 不该在 callee-saved 池里", reg);
        }
    }

    /// 池内绑定 ⇒ 两个位都置；池外绑定 ⇒ 只钉住、不动位图（今天的边界）。
    #[test]
    fn claim_sets_both_bits_only_for_pooled_registers() {
        let mut bank = RegBank::new();
        assert!(bank.claim(3), "%rbx 在池里");
        assert_eq!(bank.allocated_mask(), 0x01);
        assert_eq!(bank.used_mask(), 0x01);
        assert!(bank.is_pinned(3));

        assert!(!bank.claim(8), "%r8 不在池里");
        assert!(bank.is_pinned(8));
        assert_eq!(bank.allocated_mask(), 0x01, "池外的号不得凭空造出占用位");
        assert_eq!(bank.used_mask(), 0x01);
        assert!(!bank.is_free(8), "被钉住的号不算空闲");
    }

    /// 发号顺序 = 池下标升序，且每次都成对置两个位。
    #[test]
    fn alloc_temp_walks_pool_in_order_and_sets_both_bits() {
        let mut bank = RegBank::new();
        assert_eq!(bank.alloc_temp(), Some(3));
        assert_eq!(bank.alloc_temp(), Some(12));
        assert_eq!(bank.allocated_mask(), 0x03);
        assert_eq!(bank.used_mask(), 0x03);
        assert_eq!(bank.free_temps(), vec![14, 15]);
        assert_eq!(bank.alloc_temp(), Some(14));
        assert_eq!(bank.alloc_temp(), Some(15));
        assert_eq!(
            bank.alloc_temp(),
            None,
            "池耗尽必须报 None（调用方退回帧槽），不能复用已占的号"
        );
    }

    /// 已被绑定的号不得再被发出去（§7.15 缺陷 2 的判据形态）。
    #[test]
    fn claimed_register_is_never_issued_as_temp() {
        for step in 0..4 {
            let mut bank = RegBank::new();
            let bound = bank.pool()[step];
            bank.claim(bound);
            while let Some(r) = bank.alloc_temp() {
                assert_ne!(r, bound, "{bound} 已被变量的家占用");
            }
            let mut got: Vec<u8> = bank.pool().iter().copied().collect();
            got.remove(step);
            // 其余三个按升序被发完
            assert_eq!(
                {
                    let mut b = RegBank::new();
                    b.claim(bound);
                    let mut v = Vec::new();
                    while let Some(r) = b.alloc_temp() {
                        v.push(r);
                    }
                    v
                },
                got
            );
        }
    }

    /// 归还只清 `allocated`，绝不清 `used_by_self`；钉住的号根本不参与归还。
    #[test]
    fn release_clears_allocation_but_never_the_save_bit() {
        let mut bank = RegBank::new();
        let reg = bank.alloc_temp().unwrap(); // %rbx
        assert_eq!((bank.allocated_mask(), bank.used_mask()), (0x01, 0x01));
        assert!(bank.release(reg));
        assert_eq!(bank.allocated_mask(), 0x00, "临时值应已归还");
        assert_eq!(bank.used_mask(), 0x01, "序言那条保存不能跟着消失");

        bank.claim(reg);
        assert!(
            !bank.release(reg),
            "变量的家不是临时值：被钉住的号不允许被标回空闲"
        );
        assert_eq!(bank.allocated_mask(), 0x01);
    }

    /// 复位清空一切占用，池定义不动。
    #[test]
    fn reset_clears_occupancy_but_keeps_pool() {
        let mut bank = RegBank::new();
        bank.claim(3);
        bank.claim(8);
        bank.alloc_temp();
        bank.reset();
        assert_eq!(bank.allocated_mask(), 0);
        assert_eq!(bank.used_mask(), 0);
        assert_eq!(bank.free_temps(), vec![3, 12, 14, 15]);
        assert!(!bank.is_pinned(3));
        assert_eq!(bank.pool(), &[3, 12, 14, 15]);
    }

    /// `resync_pins` 只动钉住集合，不动两个位图；而且必须能让 `owned_gp()` 跟着
    /// 绑定表**变小**（否则 `naked` 早返窗口里会拿着上一个函数的号当成本函数的家）。
    #[test]
    fn resync_pins_tracks_the_binding_table_without_touching_masks() {
        let mut bank = RegBank::new();
        bank.claim(3); // %rbx：绑定，两个位都置
        bank.claim(8); // %r8：池外绑定，只钉住
        let before = (bank.allocated_mask(), bank.used_mask());
        assert!(bank.owned_gp().contains(&8));

        // 模拟下一个函数入口：绑定表被清空重填（这里只留 %r12）
        bank.resync_pins([12u8]);
        assert_eq!(
            (bank.allocated_mask(), bank.used_mask()),
            before,
            "resync_pins 不得顺手改位图（naked 路径上旧代码就是不改）"
        );
        let owned = bank.owned_gp();
        assert!(
            !owned.contains(&8),
            "绑定表里已经没有 %r8，排除集合必须跟着变小"
        );
        assert!(owned.contains(&12), "新绑定必须被钉住");
        // 位图没动 ⇒ %rbx 仍然占用溅出池（它不会再被当临时值发出，也不会被归还）
        assert!(bank.is_allocated(3));
    }

    /// **Phase 1 的核心等价性判据（设计 §7 1.4）**：在旧三处判据（绑定表 + 两个位图 +
    /// 规划器手工转达）的**全部可达输入组合**上，`RegBank` 给出的四个读数必须
    /// 逐位/逐元素相同。
    ///
    /// 可达集合的口径本身就是旧算法的形状：
    ///   * 临时值占用位永远是**前缀**（`alloc_callee_saved` 从下标 0 往后发号），
    ///     `0b0010` 这种「第二位单独置上」的状态在旧实现里根本造不出来 —— 第一版
    ///     把全部 16 种位模式都当输入，测出来的「漂移」是用例建模错，不是实现错；
    ///   * 绑定可以落在池的任意子集上，且池外号（用户写 `asm("r8")`）只进绑定表、
    ///     位图无位可置。
    ///
    /// 旧算法在测试里按搬迁前的字面形式重写一遍（`old_*`），实现路径已经不可能
    /// 再绕过 `RegBank` —— 这正是本 Phase 要的边界。
    #[test]
    fn regbank_readouts_are_bitwise_identical_to_the_legacy_three_judgments() {
        for n_alloc in 0usize..=4 {
            for pin_mask in 0u8..16 {
                // --- 旧状态：两个裸位图 + reg_bindings 里的号集合 ---
                // 旧 `alloc_callee_saved`：发 n 个临时值，两个位一起置；
                // 因为它是从下标 0 往后扫的，占用位必然是 (1<<n)-1 这个前缀。
                let mut old_reg_allocated = (1u8 << n_alloc) - 1;
                let mut old_used = old_reg_allocated;
                let mut old_bindings: Vec<u8> = Vec::new();
                let mut bank = RegBank::new();
                for _ in 0..n_alloc {
                    assert!(bank.alloc_temp().is_some());
                }

                // 旧绑定路径：insert 进 reg_bindings，紧跟一句 claim_bound_register。
                for i in 0..4 {
                    if pin_mask & (1 << i) != 0 {
                        let reg = POOL[i];
                        old_bindings.push(reg);
                        old_used |= 1 << i;
                        old_reg_allocated |= 1 << i;
                        bank.claim(reg);
                    }
                }
                if pin_mask & 1 != 0 {
                    // 池外的用户绑定（`asm("r8")` 形态）：只进 reg_bindings，位图无位可置
                    old_bindings.push(8);
                    bank.claim(8);
                }

                // 读数 1/2：两个位图
                assert_eq!(
                    bank.allocated_mask(),
                    old_reg_allocated,
                    "alloc={n_alloc} pin={pin_mask:04b}: allocated 位图漂移"
                );
                assert_eq!(
                    bank.used_mask(),
                    old_used,
                    "alloc={n_alloc} pin={pin_mask:04b}: callee_saved_used 位图漂移"
                );

                // 读数 3：汇编规划器的排除集合 = 旧 `owned_gp`（绑定表的号 ∪ 占用位对应的号）
                let mut old_owned: Vec<u8> = old_bindings.clone();
                for i in 0..4 {
                    if old_reg_allocated & (1 << i) != 0 {
                        old_owned.push(POOL[i]);
                    }
                }
                old_owned.sort_unstable();
                old_owned.dedup();
                assert_eq!(
                    bank.owned_gp(),
                    old_owned,
                    "alloc={n_alloc} pin={pin_mask:04b}: 规划器排除集合漂移"
                );

                // 读数 4：旧 `free_callee_saved` 的「绑定的号不还」判据（旧码现查 reg_bindings）
                for reg in POOL {
                    let old_can_free = !old_bindings.contains(&reg);
                    assert_eq!(
                        bank.release(reg),
                        old_can_free,
                        "alloc={n_alloc} pin={pin_mask:04b}: {reg} 的归还判定漂移"
                    );
                }
            }
        }
    }

    /// 分 bank 接口今天的边界：只有 GP 建模，其余通道返回空集（空集 = 尚未建模，
    /// 不是「没有寄存器」）。通道枚举本身住在 IR（`crate::ssa::location::RegBank`），
    /// 本用例同时钉住两边用的是同一个类型。
    #[test]
    fn only_gp_bank_is_modelled_for_now() {
        assert_eq!(RegBank::pool_for_bank(Bank::Gp), &[3, 12, 14, 15]);
        assert!(RegBank::pool_for_bank(Bank::Simd).is_empty());
        assert!(RegBank::pool_for_bank(Bank::FloatStack).is_empty());
        assert_eq!(RegBank::bank_of(0), Bank::Gp);
    }

    /// Phase 5.2a：被「能把控制流带出守护区间的 asm」clobber 的号不得再被发成临时值。
    ///
    /// 反向对照同时立着：池外的号（caller-saved）被 clobber 是本代码库最常见的写法，
    /// `ban` 不得为它们凭空造出限制（否则每个带 `"rcx"` clobber 的函数都会失去发号能力）。
    #[test]
    fn banned_register_is_never_issued_as_temp() {
        let mut bank = RegBank::new();
        assert!(bank.ban(3), "%rbx 在池里，ban 应命中");
        assert!(bank.is_banned(3));
        assert!(!bank.is_free(3), "被禁止的号不算空闲");
        assert_eq!(bank.free_temps(), vec![12, 14, 15]);
        assert!(!bank.ban(1), "%rcx 不在池里：不得凭空造出占用位");
        assert!(!bank.is_banned(1));
        // 发号绕开被禁止的号，其余三个按升序发完
        let mut got = Vec::new();
        while let Some(r) = bank.alloc_temp() {
            assert_ne!(r, 3, "被禁止的号不得再发出去");
            got.push(r);
        }
        assert_eq!(got, vec![12u8, 14, 15]);
        // 禁止不是占用：它不得顺手把序言的保存位打开（本函数根本没动过那个寄存器）
        assert_eq!(bank.used_mask() & 0x01, 0);
    }

    /// `held_pool_regs` 是守护集合的另一半：变量的家（`claim`）与表达式溅出槽
    /// （`alloc_temp`）都必须算「正持有值」—— 只报临时值会漏掉本阶段要修的静默错值那一类。
    #[test]
    fn held_pool_regs_covers_both_homes_and_temps() {
        let mut bank = RegBank::new();
        assert!(bank.held_pool_regs().is_empty());
        bank.claim(3); // 变量的家
        bank.alloc_temp(); // %r12（%rbx 已被占，发号往后走）
        bank.ban(15); // 未占用、仅禁止
        assert_eq!(bank.held_pool_regs(), vec![3u8, 12]);
        // 归还的临时值不再持有值；家还在
        assert!(bank.release(12));
        assert_eq!(bank.held_pool_regs(), vec![3u8]);
        assert!(!bank.held_pool_regs().contains(&15), "只被禁止的号没有值可守");
    }

    /// 禁止是逐函数事实：`reset` 必须一并清掉，不得让上一条 asm goto 的限制管到下一个函数。
    #[test]
    fn reset_lifts_the_ban() {
        let mut bank = RegBank::new();
        bank.ban(3);
        bank.ban(12);
        bank.claim(14);
        bank.reset();
        assert_eq!(bank.free_temps(), vec![3, 12, 14, 15]);
        assert!(!bank.is_banned(3));
        assert_eq!(bank.alloc_temp(), Some(3), "复位后 %rbx 必须重新可发");
    }

    /// 求值基址必须就是名字表里的那个 `%rax`（Phase 5.1 前置一）。
    ///
    /// 为什么要拿名字表对拍而不是写一句注释：`EVAL_BASE_REG` 说的是一件事（「结果落在
    /// 几号」），`abi/regnames.rs` 的 132 条表说的是同一件事的另一种表述（「这个号叫什么」）。
    /// 两边各拿一个字面量时，一侧重号（或 APX 扩表时改表）就会让「求值基址」静默变成
    /// 另一个寄存器 —— 那比错值更糟，因为全后端的发射约定都跟着它走。
    /// 本后端只支持 x86_64，号与名的对应关系就是 `att_name_of_num` 那一张表。
    #[test]
    fn eval_base_reg_is_the_name_tables_rax() {
        use crate::codegen::x86_64::abi::regnames::att_name_of_num;
        assert_eq!(
            att_name_of_num(EVAL_BASE_REG, 64).as_deref(),
            Some("%rax"),
            "求值基址必须是名字表里的 %rax，实得 {:?}",
            att_name_of_num(EVAL_BASE_REG, 64)
        );
        // 与「SysV 整型返回值在 rax」这条 ABI 事实也对得上：名字表的 0 号在
        // callee-saved 池里不得存在（池是 rbx/r12/r14/r15），否则序言会保存求值基址。
        assert!(
            !RegBank::pool_for_bank(Bank::Gp).contains(&EVAL_BASE_REG),
            "求值基址不得在 callee-saved 池里（序言会把它当变量家保存）"
        );
    }

    /// `is_call_clobbered` 的表逐号对拍（SysV AMD64），并且钉住它与本文件里另外两份
    /// 名单的关系：
    /// * `POOL`（自动分配可用的 callee-saved 池）里的号**必须**不毁 —— 否则序言保存一个
    ///   会被调用毁掉的号，而分配器以为它保得住值；
    /// * `EVAL_BASE_REG`（%rax）**必须**毁 —— 它是求值落点，跨 call 不保活本来就是它的语义。
    #[test]
    fn call_clobbered_table_matches_sysv_and_the_local_pools() {
        // 号 -> 该号是不是「不会被调用毁」的那一类（SysV AMD64 ABI 表，手写死作为独立真值）
        const CALLEE_OR_FIXED: [u8; 7] = [3, 4, 5, 12, 13, 14, 15]; // rbx rsp rbp r12..r15
        for n in 0u8..16 {
            let want = !CALLEE_OR_FIXED.contains(&n);
            assert_eq!(
                is_call_clobbered(n),
                want,
                "号 {n} 的 call-clobber 属性与 SysV 表不一致（毁={} 应为={}）",
                is_call_clobbered(n),
                want
            );
        }
        // APX 号（16+）不在已知集合里，一律当毁（宁可多报也不假装保活）
        assert!(is_call_clobbered(16), "r16 不得被当成跨 call 保活的号");
        for reg in RegBank::pool_for_bank(Bank::Gp) {
            assert!(
                !is_call_clobbered(*reg),
                "自动分配池里的号 {reg} 被说成会被调用毁 ⇒ 池与 ABI 表已经不一致"
            );
        }
        assert!(
            is_call_clobbered(EVAL_BASE_REG),
            "求值基址 %rax 必须是会被调用毁的那一类（否则表达式求值无需在 call 前存住它）"
        );
    }
}

#[cfg(test)]
mod rhs_stage_tests {
    use super::*;

    /// 取号器必须**看得见占用**：这是前置三开池的唯一前提（以前 `%rcx` 被写死成右操作数
    /// 暂存号，家给 rcx 后就被普通求值踩掉，实测 gcc 50 / dcc 47）。
    ///
    /// 判据只说「候选表」而不点名某个号：表本身单源在分配器那侧
    /// （`regalloc::AUTO_EXT_POOL`），它变过两次（五元 → 去掉 `rcx`/`rsi`/`rdi`），
    /// 而上一版本这里把首位写死成 `rcx`，表一变就测试与实现各说各话（实测报
    /// `left=8 right=9`：握着 `rcx` 根本不该换号）。
    #[test]
    fn rhs_stage_skips_regs_that_someone_holds() {
        let cands = rhs_stage_candidates();
        assert!(cands.len() >= 2, "候选表至少两个号，否则下面「换下一个」无判据");

        let mut b = RegBank::new();
        // 无人握着时取候选表首位。
        assert_eq!(b.pick_rhs_stage(0), Some(cands[0]));

        // 表**外**的号被握着不影响取号（`rcx` 是变长移位的计数，永远不进候选表）。
        // 注意 `claim` 的返回值是「在 callee-saved 池里占了位」，池外号只进 `pinned`；
        // 取号器读的是 `is_pinned`。
        assert!(!b.claim(1), "rcx 不在 callee-saved 池里，claim 不该报「池里占位」");
        assert!(b.is_pinned(1), "但池外号必须被记成握着");
        assert_eq!(b.pick_rhs_stage(0), Some(cands[0]), "握着表外的号不得改变取号结果");

        // 首位被握着时必须换到候选表里下一个空闲号。
        let mut a = RegBank::new();
        a.claim(cands[0]);
        assert!(a.is_pinned(cands[0]), "握着必须落在 pinned/allocated 上，否则取号器看不见");
        let s = a.pick_rhs_stage(0).expect("候选表还有空闲号");
        assert_ne!(s, cands[0], "{} 已被握着，取号器不得再返回它", cands[0]);
        assert_eq!(s, cands[1]);

        // dst 自己绝不能被选中：左右同归一个号会直接把操作数吃掉。
        let mut c = RegBank::new();
        for n in rhs_stage_candidates().into_iter().skip(1) {
            c.claim(n);
        }
        assert_eq!(c.pick_rhs_stage(rhs_stage_candidates()[0]), None, "只剩 dst 自己时必须报 None，由调用方去拿真正的临时号");
    }

    /// 候选表本身不得把「有主」的号收进来：这些号在别处各有硬用途（求值基址、128 位返回
    /// 高半、静态链、帧寻址 scratch）。这张表是那条排除的唯一出处，所以它自己要被检查。
    #[test]
    fn rhs_stage_candidates_exclude_regs_with_hard_uses() {
        for n in [EVAL_BASE_REG, 2u8, 4, 5, 10, 11, 13] {
            assert!(
                !rhs_stage_candidates().contains(&n),
                "号 {n} 有硬用途（求值基址/128 位返回高半/栈与帧/静态链/帧寻址 scratch），不得进暂存候选表"
            );
        }
    }
}
