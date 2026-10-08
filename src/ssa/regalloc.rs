// src/ssa/regalloc.rs
//
// P12：SSA 层的寄存器分配。
//
// # 为什么分配算在 IR 上、结果落在 AST 上
//
// 只有 IR 有真正的 CFG、phi 与单赋值形式，「某个变量在哪些点上活着」只有在 IR 上算得
// 准。而 x86_64 后端唯一被**所有**消费者承认的寄存器所有权通道是绑定表
// （`Codegen::reg_bindings`，由 `VarDecl::register_name` 驱动）：序言保存、表达式溅出
// 池、内联汇编的寄存器规划、宽度口径、asm 输出写回，现在全都读同一张表 ——
// docs/ssa-pass-upgrade-plan.md 的 §7.15 ~ §7.19 一条条闭合的就是这件事。
//
// 方案原文要求的字面形态（`ast::Ident` 携带寄存器号 + codegen 不再分配 + 真
// SPILL/RELOAD）需要重写单遍发射模型（§7.3 记了三条硬约束），不属本方案范围；这里做
// 的是它的**实质价值**：分配结果真的进入产物，并且可以用产物差异验证（§7.5 的教训）。
//
// # 为什么只做「整个生命期待在一个寄存器里」
//
// SSA→AST 重建会把同基名的多个版本塌缩回同一个可变量，读写的**先后**就是语义，所以分配
// 粒度只能是变量（基名）。这也决定判定必须偏保守：活性区间取「该基名在 IR 里最早出现的
// 位置 ~ 最晚出现的位置」，跨分支、跨回边一律按最坏情况算重叠。少绑一个变量只是少一点
// 收益，多绑一个就是静默错值。
//
// # 准入条件（每一条都对应一个实测过的失败形态）
//
// 1. 只处理局部 `Decl::Var`，且类型是 1/2/4/8 字节的标量整数或指针。
//    浮点要换 xmm 通道（后端没有为它准备的宽度口径），聚合/数组/向量本质是内存对象，
//    `long double` 是 10 字节对象，`_Atomic` 与位域另有语义。
// 2. 地址被取走过 ⇒ 不绑。判据复用 `ast_gvn::addr_escaped_names`（§7.8），绝不自己
//    再爬一遍 AST —— 两套逸出判据里弱的那一套会决定产物。
// 3. 出现在任何内联汇编的操作数里 ⇒ 不绑。asm 输出写回虽然已经认识绑定表（§7.19），
//    但 `"+r"`、`"=m"` 与「栈槽从此没人写」的组合还缺一整批实测，不把未验证的形态
//    交给自动分配。
// 4. 函数体里有嵌套函数 ⇒ 整个函数不绑：嵌套函数按帧偏移访问外层变量，而绑定变量的
//    那个偏移没人写。
// 5. `static` / `_Thread_local` / 纯 `extern` / `volatile` / `cleanup(...)` /
//    `__asm__("label")` 过的变量不动；用户已经手写 `register asm` 的名字更不动
//    （它占的那个号也从池子里扣掉）。
// 6. 开着调试信息（`-g`）⇒ 整条 pass 不动：DWARF 的变量位置目前只会发栈槽，绑上去
//    调试器会读一个从未写入的槽。这是显式记下的边界，不是遗漏 —— 要支持得发
//    DWARF 寄存器位置。
// 7. 名字在函数里被**重复声明且类型不同**（遮蔽）⇒ 不绑：后端按名字共用一个家。
// 8. 基名的**第一次出现必须是 entry 块里的定义**。两个条件缺一不可：
//    - 只看「先定义」会放过 `int x; if (c) x = 1; return x;` 这种「某条路径才初始化」
//      的变量，未初始化的那条路径就会读到**调用者留在寄存器里的值**（-O0 读到的是栈
//      垃圾）—— 同一个未定义行为在两个档位给出两个确定值，等价性门禁就变成抛硬币；
//    - 形参也不行：它的值由 ABI 送进来，在 IR 里的第一次出现是使用而不是定义。
//
//    注意 `has_init` 不能当判据用：SSA→AST 重建会把 `for (long i = 0; ...)` 发成
//    「先声明、后赋值」，实测重建后的 AST 里几乎所有局部都没有初始化式。
//
// # 可诊断
//
// `DCC_REGALLOC_DUMP=1` 打印每个函数绑了谁、绑到哪个号、以及每个被拒的名字和原因。
// 理由与 §7.6 给流水线的 `DCC_PIPE_DUMP` 一样：准入是一串保守条件，「它没跑」和「它为
// 什么没跑」在产物上长得一模一样，没有这个窗口就只能猜。

use crate::ast::{Block, BlockItem, Decl, Expr, InitItem, Stmt, Type};
use crate::ssa::{Function, LocMap, Location, RegBank, RegId, SsaVar, Splits};
use std::collections::{HashMap, HashSet};

/// 可分配的物理寄存器：x86_64 后端表达式溅出池里那四个 callee-saved 号。
///
/// 数组顺序就是分配优先级。为什么只有这四个：跨调用仍然活着的通用寄存器只有
/// callee-saved，而 `r13` 是本后端的帧基址、`rbp`/`rsp` 是帧指针与栈指针。
const POOL: [(&str, u8); 4] = [("rbx", 3), ("r12", 12), ("r14", 14), ("r15", 15)];

/// **自动分配器可用**的号：`POOL` 再加上一族 caller-saved 号（Phase 5.1 前置三）。
///
/// 为什么这一族以前不敢用：本后端当时**没有任何机制能在 call 前把寄存器里的值放到
/// 安全地方、call 后再搬回来**，所以发一个会被调用毁掉的号就是静默错值。通道现在建在
/// `abi/call.rs::emit_sysv_call` 的两端（存走 `store_stack`、取走 `load_stack`，程序点级
/// 家切换见 `Codegen::home_overrides`），所以本表里只能放**那个窗口能护住**的号。
///
/// 逐个排掉的理由（这些不是泛泛的谨慎，而是每一个号都有具体的写入点责任）：
///   * `rax(0)` 是求值基址（`EVAL_BASE_REG`），一个号不能同时是落点与某个变量的家；
///   * `rdx(2)` 是 128 位整数返回的高半（以及部分长 double 表示），存/取窗口在
///     `emit_sysv_call` 末尾，而调用方要到 `gen_call_result_move` 才把结果搬走 —— 在
///     窗口末尾就恢复 rdx 会盖掉还没被消费的高半；
///   * `r10(10)` 是嵌套函数的静态链与非局部跳转的父帧基址（`emit/stmt.rs`），它的写入
///     点不在 call 序列里；
///   * `r11(11)` 是帧内寻址的 scratch（`store_stack`/`load_stack` 的 disp32 越界分支拿它
///     算地址）—— 存/取动作自己就会毁掉它；
///   * `rsp(4)`/`rbp(5)`/`r13(13)` 固定用途（帧底/帧基）。
///
/// 剩下的 `rcx/rsi/rdi/r8/r9` 在本后端的写入点就是 SysV 参数装载与 varargs 计数，两者都
/// 在 `emit_sysv_call` 内部，所以这个窗口的覆盖面是完整的。这个假设不靠口头保证：
/// 错了会直接体现在 `check_opt_equivalence.sh`、夹具执行对拍与 gcc 交叉对拍上（本轮已跑）。
/// 剩下的号里，**`rcx` 也不能当家**：x86 的变长移位/循环移位把计数**强制放在 `%cl`**
/// （`shl/shr/sal/sar/ror/rol` 的 `r/m, %cl` 形式，以及 `__builtin_rotateright8` 这类
/// 内置）。它不是一个可以「用前存、用后取」的普通暂存位：任何一条变长移位都会写 `%cl`。
/// 实测证据：开池后 `tests/fixtures/c/rotate.c` 在 -O2 下第 4 项失败
/// （`__builtin_rotateright8(0xC1, 2) != 0x70`），而同一夹具不开池返回 0。
/// 所以本表只放 `{rsi, rdi, r8, r9}`；`rcx` 归发射侧当暂存候选（它永远不会是家，
/// 于是两个角色不再挤同一批号 —— 这一条比 `stage_free_after` 更根本）。
/// 实测逐号定案（`DCC_EXT_POOL` 按号开，判据是四档执行对拍）：
///   * `rcx` ✕ —— x86 变长移位/循环移位的计数强制在 `%cl`（`rotate.c` 开池后第 4 项
///     `__builtin_rotateright8(0xC1,2)` 错），不是可「用前存、用后取」的普通位；
///   * `rsi`/`rdi` ✕ —— 它们同时是 `rep movsb/rep movsq` 的源与目的（聚合拷贝/内存内置
///     在**非调用路径**上就会重写这两个号），`strcasecmp.c` 开池后第 5 项错、只开
///     `{r8,r9}` 时三夹具全部与关池同值；
///   * `r8`/`r9` —— 以前记的两组读数（`DCC_EXT_POOL=r8,r9` 报 **OPT-DIFF 177**，只开
///     `{rsi,rdi,r8,r9}` 时是 5）**已作废**：修完比较臂那条 REX.B 漏发之后重测，默认
///     关池在同一个 364 语料上是 **0** 条，开 `r8,r9` 是 **2** 条（`c_strcasecmp.c` 的
///     -O2/-O3）+ 3 条新增 COMPILE-FAIL（`attr_target_clones_cpu_*`）。⇒ 那 177/5 量的
///     是同一条编码缺陷，不是池的代价；证据与开池前置判据见设计 §11.53/§11.54。
///
/// 因此延伸池默认仍不开（`ext_pool_selection()` 默认空），且不得因为本表非空就认为安全。
const AUTO_EXT_POOL: [(&str, u8); 2] = [("r8", 8), ("r9", 9)];

/// 自动分配器的完整候选表。
///
/// **默认只等于 `POOL`：延伸池发号已被实测否决**（原因与设计结论见本文 §11.39）。形状：
/// `int e = sink(5)` 的家给成 `%rcx` 后，紧跟其后的求值把 `%rcx` 当二元运算右操作数的
/// 固定暂存号（`emit/expr/binary.rs` 的 `gen_expr_decay(r, 1)` 与那批 rcx 专属字节），家
/// 就被普通计算覆盖：-O2 下 gcc 给 50、dcc 给 47。
///
/// 但「哪一个号真的危」不应该靠读代码的直觉回答。本函数按 `DCC_EXT_POOL` 的逗号列表
/// 把对应的号加进候选表（默认空 ⇒ 行为与今天逐字相同），于是可以**逐号**在真产物上跑
/// 执行对拍定案（审计入口：`.work_tmp/p12_base/audit_ext_pool.sh`）。这不是第二行为开关，
/// 而是量具：产物路径上它永远关着，因为发布构建不会设这个环境变量（`ext_pool_enabled`
/// 的默认值就是空，而且有用例钉住「不开池时不得发出延伸池家」）。
pub fn ext_pool_selection() -> Vec<u8> {
    let spec = match std::env::var("DCC_EXT_POOL") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    spec.split(',')
        .map(|t| t.trim().trim_start_matches('%').to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .filter_map(|t| AUTO_EXT_POOL.iter().find(|(nm, _)| *nm == t).map(|(_, r)| *r))
        .collect()
}

fn auto_pool() -> Vec<(&'static str, u8)> {
    let mut out: Vec<(&'static str, u8)> = POOL.iter().map(|&(n, r)| (n, r)).collect();
    for n in ext_pool_selection() {
        if let Some(&(nm, r)) = AUTO_EXT_POOL.iter().find(|(_, v)| *v == n) {
            if !out.iter().any(|(_, v)| *v == r) {
                out.push((nm, r));
            }
        }
    }
    out
}

/// 延伸池在不在自动候选表里，只由这一个开关回答（诊断与用例都读它，不各自复制形状）。
pub fn ext_pool_enabled() -> bool {
    !ext_pool_selection().is_empty()
}

/// 发射侧的暂存候选表（`codegen::x86_64::abi::regbank::RHS_STAGE_CANDIDATES`）就是本表：
/// 两个模块各自需要知道同一件事，但只有这里能持有名单。给用例用。
pub fn auto_ext_pool_regs() -> Vec<u8> {
    AUTO_EXT_POOL.iter().map(|&(_, r)| r).collect()
}

/// 这个号是不是需要存/取通道的延伸池号（给发射侧用：只有这类家才预留保存槽）。
pub fn is_auto_ext_reg(n: u8) -> bool {
    AUTO_EXT_POOL.iter().any(|&(_, r)| r == n)
}

/// 「自动池满了」这个拒因的**唯一字串**。以前它以内联文本抄在 5 个地方（1 个推 + 4 个
/// 读），而度量（`pool_full_rejects`/`shareable`/`nocall`）与分配器共用这个键：抄本一旦
/// 不一致，账面上的收益面就会静默变成 0，而那看上去像「没机会」而不是「算错了」。
pub const REASON_NO_FREE_REG: &str = "no-free-auto-assignable-register";

/// `DCC_REGALLOC_DUMP=1` 时打印诊断（缓存一次，避免每条 pass 都读环境）。
fn dump_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("DCC_REGALLOC_DUMP").as_deref() == Ok("1"))
}

/// 一个函数的分配结果。
#[derive(Debug)]
pub struct Plan {
    /// 变量基名 → 寄存器名（`register asm` 的写法，如 `"rbx"`）
    pub bindings: Vec<(String, &'static str)>,
    /// 被拒的名字与原因（只为 dump 与测试服务；顺序确定）
    pub rejected: Vec<(String, &'static str)>,
    /// 量化读数（`docs/p12-physreg-spill-design.md` §7 的 Phase 0.2）：只服务诊断与
    /// 「值不值得做字面形态」的判定，**不参与任何分配决定**，因此产物零影响；
    /// 仅在 `DCC_REGALLOC_DUMP=1` 时才计算（`max_overlap` 是 O(n²)）。
    pub stats: PlanStats,
    /// 版本粒度的位置表（P12 Phase 3）。**从 Phase 3.2 起它是写进 AST 的唯一输入**；
    /// `bindings` 退为诊断/测试读数，两者不一致时单测
    /// `the_ast_landing_comes_from_the_table` 会红。
    ///
    /// 本类型因此不再 `derive(Clone, Default)`：`LocMap` 故意不可复制也不默认构造
    /// （见 `ssa/location.rs`），「忘了建表」与「建了但空」不得长得一样。
    pub loc: LocMap,
}

impl Plan {
    /// 显式空表（取代旧的 `Plan::default()`：`LocMap` 不实现 `Default` 是有意的）。
    fn empty() -> Self {
        Self {
            bindings: Vec::new(),
            rejected: Vec::new(),
            stats: PlanStats::default(),
            loc: LocMap::empty(),
        }
    }
}

/// Phase 0 要求量化的两个数：池满造成的拒绑占比，与 IR 上同时活标量的上界。
/// 前者是 Phase 5（真 SPILL/RELOAD）的收益上界；如果它接近 0，溢出收益就只是
/// 纸面收益（设计 §7 Phase 0 的判据原文）。后者是「池有多大才够」的实测口径。
#[derive(Debug, Default, Clone, Copy)]
pub struct PlanStats {
    /// 走完全部准入判据、进入「挑号」环节的候选数
    pub pool_candidates: usize,
    /// 因为池里没有空闲号而被拒的候选数
    pub pool_full_rejects: usize,
    /// IR 里所有可见基名的 `[first, last]` 区间最大重叠数
    pub max_live_bases: usize,
    /// 同上，只算进入挑号环节的候选
    pub max_live_candidates: usize,
    /// **池满拒绑里有多少是「版本分裂能救」的**（其余只能靠溢出救）。
    ///
    /// 分法根据是干涉表：被池满拒的候选如果与某个已绑基名存在一对**不同时活着**的
    /// 版本，那今天挡住它的就不是物理干涉，而是「一个号只给一个变量」这条 blanket
    /// 规则（§7.20 判据 3）。这个数直接决定 Phase 5 的工力该投在 spill 还是在分裂上
    /// —— 设计 §7 Phase 0 的原话：没有下降数据的收益不算收益。
    pub pool_full_shareable: usize,
    /// **池满被拒的候选里，有多少个活区间不跨 call**（Phase 5.1 前置三的收益面）。
    ///
    /// 这一族是「给它们发 caller-saved 号就立刻能用、不需要任何保存通道」的部分；
    /// 剩下的那部分才需要前置三的 store/load 通道。它和 `pool_full_shareable` 回答不
    /// 同一个问题：后者问「callee-saved 池里能不能挤出一个号」，前者问「能不能用池外的号」。
    pub poolfull_no_call: usize,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// 用户手写的 `register asm` 用的是不是池子里的号（是则该号已被占用）。
pub fn pooled_reg_of(asm_name: &str) -> Option<u8> {
    let n = asm_name.trim_start_matches('%').to_ascii_lowercase();
    POOL.iter()
        .find(|(nm, _)| *nm == n)
        .map(|(_, r)| *r)
}

/// 号 → `register asm` 的写法。**自动池整张表都是候选**（callee-saved + 延伸池）：以前
/// 只查 `POOL`，于是分配器给的延伸池家在这里变成 `None`，被归到
/// 「不可表示」而静默退回内存 —— 那看上去只在绑定读数里有，到不了产物。
fn pool_reg_name(n: u8) -> Option<&'static str> {
    auto_pool().into_iter().find(|(_, v)| *v == n).map(|(nm, _)| nm)
}

/// 名 → **整个自动池**里的号（callee-saved 加延伸池）。与 `pooled_reg_of` 不是一回事：
/// 后者只回答「这个名是不是需要序言保存的那族号」，而本函数回答「分配器能不能拿它当家」。
/// 两者分开写而不是共用 `pooled_reg_of`，是因为前置三以后自动绑定可以落在 callee-saved
/// 池外：用错了就会把延伸池的绑定静默丢掉（实测就是这个形状：绑定读数里有、产物里没有）。
pub fn auto_reg_of(asm_name: &str) -> Option<u8> {
    let n = asm_name.trim_start_matches('%').to_ascii_lowercase();
    auto_pool().into_iter().find(|(nm, _)| *nm == n).map(|(_, r)| r)
}

/// 调用方需要的上下文。
#[derive(Debug, Clone)]
pub struct Ctx {
    /// 本次编译要不要发调试信息（`-g*`）
    pub debug_info: bool,
    /// file-scope `register asm` 变量占住的号
    pub taken_regs: HashSet<u8>,
}

/// 算出该绑哪些变量，并把结果写进**重建后的** AST。
///
/// `judge_on` 是**重建前**的函数体，`body` 是重建后的：判据必须看前者。实测
/// （`tests::nested_function_is_detected_from_the_original_body`）重建后的函数体里
/// 已经没有嵌套函数的 `Decl::Func`（它被前面的降低步骤搬走了），只在重建后的体上找
/// 就会漏掉「外层变量被嵌套函数按帧偏移访问」这一整类，而那一类绑上去是静默错值。
/// 两边都扫，判据取并集（保守），落地只写进 `body`。
///
/// **Phase 3.2**：分配结果同时写进 `func.value_loc`，且 AST 落地**只经由这张表**；
/// 函数签名从 `&Function` 改成 `&mut Function` 就是这件事的形状证据 —— 位置表是 IR 的
/// 一部分，不是调用方事后自己拼的副产品。
/// 决策阶段（Phase 5.0）：只看 IR 与**重建前**的体，并把结果写进 `func`。
///
/// 为什么要拆成两步：版本分裂表必须在 `rebuild_block` **之前**就存在于 IR 上，重建才能
/// 给不同版本生成各自的局部（见设计 §11.8.1 记录的那个循环）。而旧形状把决策放在重建
/// 之后，还多扫一份重建后的体 —— 那等于把决策建立在“重建会产生什么”上，永远无法前置。
///
/// 旧的那份后置扫描不能直接丢：丢了就是未可证明的行为变更。所以它拆到 `validate_against_rebuilt`
/// 里作为**校验**跑（不改决策），并由用例钉住“后置事实不改变计划”（§11.9 的探针实测：
/// 在 291 份夹具上去掉后置扫描，绑定结果逐字节不变）。
pub fn plan_and_split(func: &mut Function, original: &Block, ctx: &Ctx) -> Plan {
    let plan = plan_for(func, original, ctx);
    // D3 / Phase 3.4：同基名的版本拿到不同家时，它们必须成为各自的 AST 局部；
    // 表算一次存进 IR，重建与交付读同一个答案。今天总是空表 ⇒ 产物不变。
    let splits = plan.loc.compute_splits(func);
    func.local_splits = splits.clone();
    let loc = plan.loc.fork();
    func.value_loc = loc;
    plan
}

/// 交付阶段（重建之后）：校验后置事实，然后**只经由位置表**把结果写进重建后的 AST。
///
/// 不回滚决策：一旦“后置发现→取消已算出的家”，分裂表与实际发射就会不一致（多一个
/// 声明却没家的局部会改变帧内布局）。校验只负责把这类事说出来，让判据能往前修。
pub fn deliver(func: &Function, plan: &mut Plan, body: &mut Block) {
    validate_against_rebuilt(func, plan, body);
    if !plan.loc.is_empty() {
        apply_bindings(body, plan, &func.local_splits);
    }
    if dump_on() {
        eprint!("[regalloc] {}\n", func.name);
        for (n, r) in &plan.bindings {
            eprint!("  bind {n} -> %{r}\n");
        }
        for (n, why) in &plan.rejected {
            eprint!("  skip {n}: {why}\n");
        }
        // 量化行：Phase 0 的「值不值得干」与 Phase 5 的归属数据都从这里取（全语料聚合）。
        eprint!(
            "  stats cands={} poolfull={} live={} live_cands={} shareable={} nocall={} needssave={}\n",
            plan.stats.pool_candidates,
            plan.stats.pool_full_rejects,
            plan.stats.max_live_bases,
            plan.stats.max_live_candidates,
            plan.stats.pool_full_shareable,
            plan.stats.poolfull_no_call,
            plan.stats.pool_full_rejects.saturating_sub(plan.stats.poolfull_no_call)
        );
    }
}

/// 后置事实在已绑名字上的冲突列表（`(基名, 原因)`）。抽成返回值而不是直接打印，
/// 是为了它能被用例直接断言（而不是靠 grep 诊断文本）。
fn post_rebuild_conflicts(plan: &Plan, rebuilt: &Block) -> Vec<(String, &'static str)> {
    let mut post = Scan::default();
    scan_block(rebuilt, &mut post);
    let escaped_post = crate::ast_gvn::addr_escaped_names(rebuilt);
    let mut out = Vec::new();
    for (base, _) in &plan.bindings {
        // 不查 `has_nested_fn`：嵌套函数在重建后的体里已经消失（§7.20 判据 1），
        // 所以它只能来自治理前的体，当后置信号会永远不成立。
        let why: Option<&'static str> = if escaped_post.contains(base) {
            Some("address-escaped")
        } else if post.asm_names.contains(base) {
            Some("inline-asm-operand")
        } else if post.infos.get(base.as_str()).map_or(false, |i| i.user_bound) {
            Some("already-register-asm")
        } else {
            None
        };
        if let Some(w) = why {
            out.push((base.clone(), w));
        }
    }
    out.sort();
    out
}

/// 校验：重建后的体里如果出现了「会让已绑名字不该绑」的新事实，就是决策前置的假设
/// 破了。这种事必须是**可看见的**，而不是默默改变绑定（后者会把帧布局也拖动）。
///
/// 为什么不用 `report_internal_bug`：那会把一条“当前未证实但不改变行为”的不变式违反
/// 直接变成编译失败；先走诊断窗口，等语料上零命中后再升级成硬错误（升级只改本函数）。
fn validate_against_rebuilt(func: &Function, plan: &Plan, rebuilt: &Block) {
    for (base, w) in post_rebuild_conflicts(plan, rebuilt) {
        eprintln!(
            "dcc: regalloc 后置校验命中：{} 里的 '{}' 只在重建后的体里露出 '{}'（决策已前置，不回滚）",
            func.name, base, w
        );
    }
}

/// 旧的单一入口（保留给测试：决策 + 交付一次做完，`body` 就是重建后的体）。
pub fn assign(func: &mut Function, original: &Block, body: &mut Block, ctx: &Ctx) -> Plan {
    let mut plan = plan_and_split(func, original, ctx);
    deliver(func, &mut plan, body);
    plan
}

// ---------------------------------------------------------------------------
// 规划
// ---------------------------------------------------------------------------

/// 变量的静态信息（一次 AST 遍历收齐）。
struct VarInfo {
    ty: Type,
    bindable_ty: bool,
    plain_local: bool,
    user_bound: bool,
}

struct Cand {
    name: String,
    first: i32,
    last: i32,
    accesses: usize,
}

/// D5：**能不能把家定在物理寄存器**的唯一判据（P12 Phase 4.3 把守卫从 pass 入口
/// 搬到这里）。
///
/// 旧写法是 `plan_for` 开头 `if ctx.debug_info { return plan; }`，语义是「pass 不跑」；
/// 而真正的规则是「本函数不允许产出 `Reg` 家，因为 DWARF 只会发栈槽位置，绑上去就是
/// 让调试器读一个从未被写入的槽」。两者当时产物相同，但下阶段不同：Phase 6.2 给
/// DWARF 补上寄存器位置表达式后，取消限制只需改本函数一处，而不必重新找一个
/// 「pass 入口的跳过点」并保证它的判据与 Location 一致。
///
/// **Phase 6.2 已接线，所以这里不再拦**：`src/debug.rs` 会从 `Codegen::debug_var_facts`
/// 给每个有物理寄存器家的局部发 `DW_OP_regN`（N 由号表换算，实测 gdb `print` 读到的是
/// 活寄存器本身：改掉 %rbx，`print v` 立刻跟着变）。`POOL` 里只有 callee-saved
///（rbx/r12/r14/r15），后端是**函数级**的名字→号绑定（一个号一个变量，见下面那段
/// 为什么不按区间共用），所以上面那个「只发栈槽位置」的前提不在了。
///
/// 保留本函数而不是删掉：它是那条规则的**唯一**落点，以后谁要重新拦（比如发现某个
/// 形状的位置表达式仍发不出）只需改这里，不必再回到 pass 入口去找早返点。返回恒为
/// `true` 是刻意的：`ctx` 参数不读 `debug_info` 了，而那个字段仍然留着（`src/opt.rs` 构
/// 造 `Ctx` 时需要它，且它是 `-g` 在本模块里唯一的可观测身份）。
fn register_homes_allowed(_ctx: &Ctx) -> bool {
    true
}

fn plan_for(func: &Function, original: &Block, ctx: &Ctx) -> Plan {
    let mut plan = Plan::empty();
    // D5 的守卫不再在这里早返（见 `register_homes_allowed`）：判据照跑，只是不产出 Reg 家。
    let homes_allowed = register_homes_allowed(ctx);
    let mut sc = Scan::default();
    // 决策阶段只看**重建前**的体（Phase 5.0）。重建后那份事实由 `validate_against_rebuilt`
    // 负责，它不再参与决定谁绑、绑到哪个号。
    scan_block(original, &mut sc);
    let escaped = crate::ast_gvn::addr_escaped_names(original);
    // 用户手写的 `register asm`（局部或 file-scope）占住的号一律让开。光把那个名字
    // 记进 rejected 是不够的：后端照样会把同一个号给变量，两个变量共享一个寄存器
    // 就是静默错值 —— 这条是 `tests::user_bound_local_reserves_its_register` 逼出来的。
    let mut taken = ctx.taken_regs.clone();
    taken.extend(sc.user_bound_regs.iter().copied());
    let infos = &sc.infos;
    let spans = base_spans(func);

    // 候选者必须按**完全确定**的顺序处理：候选集来自 HashMap，排序键取
    // 「访问次数降序 → 区间起点升序 → 名字升序」，寄存器按 POOL 顺序取第一个不冲突的。
    let mut cands: Vec<Cand> = Vec::new();
    let mut names: Vec<&String> = infos.keys().collect();
    names.sort();
    for name in names {
        let info = &infos[name];
        // 判据先算，**再**看 IR 里有没有这个基名：顺序反了会让「IR 里根本没出现过」
        // 的名字（典型是被取址的变量、以及 volatile 读写被降级成 `_tnload_*` 临时值的
        // 变量）既不进 bindings 也不进 rejected，等于凭空消失 —— dump 与测试都看不见它
        // 为什么没被绑，而这正是本 pass 唯一可诊断的入口。
        // 候选者必须是**用户声明的局部**（`original_locals`）。不加这条的话，重建
        // 出来的编译器临时值（`_tbin_*`、`_tcall_*` 之类）也会被当成变量去绑 ——
        // 实测 `seg_qualifier.c` 的 main 一度把四个临时值全绑走了：既挤掉了真正的热
        // 变量，又把「合成临时值」这条后端专门有约定（`is_synthetic_temp_base`，见
        // §7 的第 5 轮补记）的名字推进了绑定表。
        let why: Option<&'static str> = if !func.original_locals.contains(name) {
            Some("not-a-user-local")
        } else if sc.has_nested_fn {
            Some("nested-function-in-scope")
        } else if !info.bindable_ty {
            Some("type-not-bindable")
        } else if !info.plain_local {
            Some("static/tls/extern/volatile/atomic/cleanup/asm-label")
        } else if info.user_bound {
            Some("already-register-asm")
        } else if sc.shadowed.contains(name) {
            Some("shadowed-with-different-type")
        } else if escaped.contains(name) {
            Some("address-escaped")
        } else if sc.asm_names.contains(name) {
            Some("inline-asm-operand")
        } else {
            match spans.get(name) {
                // IR 里没有这个基名的任何读写：没有生命期可言，也就无从判断
                // 「定义是否在 entry 块」，一律不绑。
                None => Some("no-ir-presence"),
                Some(sp) if !sp.entry_def => Some("not-defined-in-entry-block"),
                Some(_) => None,
            }
        };
        match why {
            Some(w) => plan.rejected.push((name.clone(), w)),
            None => {
                let sp = &spans[name];
                cands.push(Cand {
                    name: name.clone(),
                    first: sp.first,
                    last: sp.last,
                    accesses: sp.defs + sp.uses,
                });
            },
        }
    }
    cands.sort_by(|a, b| {
        b.accesses
            .cmp(&a.accesses)
            .then(a.first.cmp(&b.first))
            .then_with(|| a.name.cmp(&b.name))
    });

    // 一个号只发给一个变量，绝不复用。
    //
    // 为什么不能按「区间不相交就共用号」（我第一版就是这么写的，实测两处错值）：
    // 后端的绑定表是**函数级**的 `名字 -> 号`（`Codegen::reg_bindings`），读写路径
    // 每次都查同一张表，中间没有任何「重新绑到别的号」的时机。两个名字指向同一个号
    // 就是同一个家被两人写，跟它们的 IR 区间是否重叠无关。实测形态：
    //   char16_t c16 = 'A'; char32_t c32 = U'B'; if (c16 != 65) ...
    //   SCCP 把比较折掉之后 `c16` 在 IR 里只剩定义那一个位置，区间退化成点，
    //   于是被判成「与 c32 不相交」而共用 %rbx —— -O2 下 rc=3（gcc 是 0）。
    // 结论：这个后端的架构决定了分配粒度是「整个函数」，区间只用来排序与选择，
    // 不用来共用寄存器。
    let mut used_slots: Vec<usize> = Vec::new();
    for c in &cands {
        if !homes_allowed {
            // 本阶段不拦（`register_homes_allowed` 恒为 true）。保留这条分支形状是为了
            // 让「拦」与「拒因可诊断」同时成立：谁重新拦上，名字仍然会带原因进
            // `rejected`，而不是退回到「看不到为什么没绑」。
            plan.rejected.push((c.name.clone(), "register-home-disallowed-by-location-policy"));
            continue;
        }
        // 发号前先算出「哪些号已经归属某个家」：本次检查需要它，而不能到发射期才发现全满。
        //
        // 「必须给右操作数暂存留至少一个空闲候选」是**发号判据的一部分**，不是发射期的兑底：
        // 延伸池与暂存候选是同一批号（§11.47 实测：候选表全被家占满时 `emit` 会
        // 报 `E1135`），而只有分配器能看见所有候选的最终归属 —— 到了发射期已经无从选择。
        let pool = auto_pool();
        let held_by_pool: Vec<u8> = pool
            .iter()
            .enumerate()
            .filter(|(i, _)| used_slots.contains(i))
            .map(|(_, (_, r))| *r)
            .collect();
        let stage_free_after = |rv: u8| -> bool {
            // 「延伸池号」与「右操作数暂存候选」就是同一批 5 个号（两边的定义各自
            // 排掉了 rax/rdx/r10/r11，由 `auto_ext_pool_is_the_staging_candidate_set` 钉住这个
            // 相等关系）。所以这里用本地的 `AUTO_EXT_POOL` 表达即可，不需要跨层去读后端的
            // 候选表 —— 而且跨层也读不到（`codegen::x86_64::abi` 是私有模块）。
            !is_auto_ext_reg(rv)
                || AUTO_EXT_POOL
                    .iter()
                    .any(|&(_, c)| c != rv && !taken.contains(&c) && !held_by_pool.contains(&c))
        };
        let pick = pool
            .iter()
            .enumerate()
            .find(|(i, (_, rv))| {
                !taken.contains(rv) && !used_slots.contains(i) && stage_free_after(*rv)
            })
            .map(|(i, (nm, _))| (i, *nm));
        match pick {
            Some((i, nm)) => {
                used_slots.push(i);
                plan.bindings.push((c.name.clone(), nm));
            },
            // 自动池满了：callee-saved 与延伸池一个都不剩（或剩下的会让暂存耗空）。
            None => plan
                .rejected
                .push((c.name.clone(), REASON_NO_FREE_REG)),
        }
    }
    // 位置表（版本粒度）：从已定稿的 bindings + IR 建，供 `assign` 落地并由 IR 携带。
    let loc = to_loc_map(&plan, func);
    plan.loc = loc;
    // Phase 0 的量化读数（`docs/p12-physreg-spill-design.md` §7 0.2）。只服务于
    // `DCC_REGALLOC_DUMP` 的统计，**不参与任何分配决定**，因此产物零影响；
    // 并且 `max_overlap` 是 O(n²)，所以只在诊断开着时算 —— 不能把一次性探针
    // 变成每次 -O2 每个函数都要付的固定成本。
    if dump_on() {
        plan.stats.pool_candidates = cands.len();
        plan.stats.pool_full_rejects = plan
            .rejected
            .iter()
            .filter(|(_, w)| *w == REASON_NO_FREE_REG)
            .count();
        plan.stats.max_live_bases =
            max_overlap(spans.values().map(|s| (s.first, s.last)).collect::<Vec<_>>());
        plan.stats.max_live_candidates =
            max_overlap(cands.iter().map(|c| (c.first, c.last)).collect());
        // 干涉分桶：池满被拒的候选里，有多少本可以复用某个已绑号（只是版本不重叠）。
        // 这张表只在诊断开着时算：它是 Phase 5 的决策数据，不是今天的分配输入。
        plan.stats.pool_full_shareable = shareable_rejects(func, &plan);
        // 前置三收益面：池满被拒的候选里，有多少根本不过 call（这些用 caller-saved
        // 号不需要保存通道）。同样只在诊断开着时算，不是今天的分配输入。
        plan.stats.poolfull_no_call = poolfull_no_call(func, &plan);
    }
    plan
}

/// 池满被拒的候选里，活区间**不跨 call** 的个数（前置三的收益面，只读诊断）。
fn poolfull_no_call(func: &Function, plan: &Plan) -> usize {
    plan.rejected
        .iter()
        .filter(|(_, why)| *why == REASON_NO_FREE_REG)
        .filter(|(name, _)| !crate::ssa::liveness::spans_call(func, name))
        .count()
}

/// 「池满被拒的候选里，有多少本可以复用某个已绑号」的计数（设计 §7 Phase 0 的归属数据）。
///
/// 抽成函数而不是内联在诊断分支里，是为了它能被用例直接验证 —— 一个「0」既可能是
/// 真结论（没机会），也可能是度量本身算错；不验证就分不清，而这两个结论会把 Phase 5
/// 的工力引到相反的方向上（分表 vs 溢出）。
pub(crate) fn shareable_rejects(func: &Function, plan: &Plan) -> usize {
    let iv = crate::ssa::liveness::compute(func);
    let bound: Vec<String> = plan.bindings.iter().map(|(b, _)| b.clone()).collect();
    if bound.is_empty() {
        return 0;
    }
    let mut shareable = 0usize;
    for (cand, why) in &plan.rejected {
        if *why != REASON_NO_FREE_REG {
            continue;
        }
        let cv = crate::ssa::liveness::versions_of_base(func, cand);
        if cv.is_empty() {
            continue;
        }
        let ok = bound.iter().any(|b| {
            let bv = crate::ssa::liveness::versions_of_base(func, b);
            // 用 ∀ 的 `ranges_disjoint` 而不是 ∃ 的 `bases_have_disjoint_pair`：
            // 基名在重建后只有一个格子，只要有一对版本同时活着就不能共用一个号。
            // 上一轮用 ∃ 算出的 58 因此高估了收益（设计 §11.9 已订正）。
            !bv.is_empty() && iv.ranges_disjoint(&cv, &bv)
        });
        if ok {
            shareable += 1;
        }
    }
    shareable
}

/// 闭区间 `[first, last]` 的最大重叠数（含区间自身，所以非空集合的上界至少为 1）。
///
/// 这是**上界口径**而非逐点活性：真正的逐点活性要跑 CFG liveness（那是 Phase 5
/// 干涉表的事），而 Phase 0 要回答的是「池子尺寸与同时活标量数的量级差距」，
/// 基名区间就够。O(n²) 在语料规模上无所谓。
fn max_overlap(spans: Vec<(i32, i32)>) -> usize {
    let mut best = 0usize;
    for (a, b) in &spans {
        let mut n = 0usize;
        for (c, d) in &spans {
            if c <= b && a <= d {
                n += 1;
            }
        }
        best = best.max(n);
    }
    best
}

// ---------------------------------------------------------------------------
// AST 遍历：变量声明、asm 操作数、嵌套函数
// ---------------------------------------------------------------------------

/// 该类型能不能绑进 GP 寄存器（见文件头准入条件 1）。
///
/// 只放**平台标准整型与指针**：`char/short/int/long/long long` 及其无符号形态，加上
/// 指针。以下都不交给自动分配，理由不是「怕麻烦」而是**后端对它们另有通路**，而
/// 绑定路径只保证与栈槽路径共用 `var_access_shape` 那一条口径：
///   * `char8_t/char16_t/char32_t`、`_Bool`：字符/布尔宽度另有约定，实测夹具
///     `char16_char32.c` 就是这条 pass 的第一版在这里做出错值的（另一个原因是同号
///     复用，见 `plan_for`）；
///   * 浮点（含 `long double`）、复数、向量：值在 xmm/x87 上；
///   * 数组、结构体、联合体：它们本身就是内存对象；
///   * `_Atomic`：原子访问必须留在内存对象上。
/// 收窄的代价是少绑几个变量，放宽的代价是静默错值 —— 这个方向上不能含糊。
fn bindable_ty(t: &Type) -> bool {
    match t {
        // 指针可以绑（就是一个 8 字节槽）；`Func` 类型与 `_Atomic` 不绑：前者是函数
        // 设计算器语义的载体，后者的读写必须留在内存对象上。
        Type::Func(..) | Type::Atomic(_) => false,
        other => {
            let ok = matches!(
                other,
                Type::Char
                    | Type::SChar
                    | Type::UChar
                    | Type::Short
                    | Type::UShort
                    | Type::Int
                    | Type::UInt
                    | Type::Long
                    | Type::ULong
                    | Type::LongLong
                    | Type::ULongLong
            );
            ok && other.size() <= 8
        },
    }
}

/// 一次 AST 遍历收集的四样东西（都是「能不能绑」的判据）。
#[derive(Default)]
struct Scan {
    /// 变量基名 → 静态信息
    infos: HashMap<String, VarInfo>,
    /// 同名但类型不同的名字（遮蔽）
    shadowed: HashSet<String>,
    /// 出现在内联汇编操作数里的根对象名
    asm_names: HashSet<String>,
    /// 函数体里有没有嵌套函数
    has_nested_fn: bool,
    /// 用户手写 `register asm` 占住的、属于本 pass 池子的号
    user_bound_regs: HashSet<u8>,
}

fn scan_block(b: &Block, sc: &mut Scan) {
    for item in &b.items {
        match item {
            BlockItem::Decl(Decl::Var(v)) => {
                let plain_local = !v.is_static
                    && !v.is_thread_local
                    && !(v.is_extern && v.init.is_none())
                    && !v.quals.is_volatile()
                    && v.cleanup_fn.is_none()
                    && v.asm_label.is_none()
                    && v.alias_name.is_none()
                    // 段前缀变量（`__seg_gs` / `__seg_fs`，Linux per-cpu 与 TLS）的
                    // 读写走的是「段基址 + 帧偏移」这条完全独立的路径；一旦绑到寄存器，
                    // 读写两边就会一个走寄存器、一个走段内存。实测 `seg_qualifier.c`
                    // 在 -O2 下 rc 从 37 变 67。
                    && v.addr_space == crate::ast::AddrSpace::None
                    && v.tls_model.is_none()
                    && !matches!(v.ty, Type::Atomic(_));
                let fresh = VarInfo {
                    ty: v.ty.clone(),
                    bindable_ty: bindable_ty(&v.ty),
                    plain_local,
                    user_bound: v.register_name.is_some(),
                };
                let name = v.name.name.clone();
                match sc.infos.get_mut(&name) {
                    // 同名重复声明：类型不同就是遮蔽，后端只会给它一个家。
                    Some(prev) => {
                        if prev.ty != fresh.ty {
                            sc.shadowed.insert(name);
                        }
                        prev.plain_local &= fresh.plain_local;
                        prev.user_bound |= fresh.user_bound;
                        prev.bindable_ty &= fresh.bindable_ty;
                    },
                    None => {
                        sc.infos.insert(name, fresh);
                    },
                }
                if let Some(rn) = &v.register_name {
                    if let Some(r) = pooled_reg_of(rn) {
                        sc.user_bound_regs.insert(r);
                    }
                }
                if let Some(e) = &v.init {
                    scan_expr(e, sc);
                }
            },
            BlockItem::Decl(Decl::Func(f)) => {
                if f.body.is_some() {
                    sc.has_nested_fn = true;
                }
            },
            BlockItem::Stmt(s) => scan_stmt(s, sc),
            _ => {},
        }
    }
}

fn scan_stmt(s: &Stmt, sc: &mut Scan) {
    match s {
        Stmt::Block { b, .. } => scan_block(b, sc),
        Stmt::Expr { e, .. } => scan_expr(e, sc),
        Stmt::Return { val, .. } => {
            if let Some(e) = val {
                scan_expr(e, sc);
            }
        },
        Stmt::If {
            cond, then_, else_, ..
        } => {
            scan_expr(cond, sc);
            scan_stmt(then_, sc);
            if let Some(x) = else_ {
                scan_stmt(x, sc);
            }
        },
        Stmt::While { cond, body, .. } | Stmt::Do { cond, body, .. } => {
            scan_expr(cond, sc);
            scan_stmt(body, sc);
        },
        Stmt::For {
            init,
            cond,
            step,
            body,
            ..
        } => {
            if let Some(i) = init {
                scan_stmt(i, sc);
            }
            if let Some(c) = cond {
                scan_expr(c, sc);
            }
            if let Some(st) = step {
                scan_expr(st, sc);
            }
            scan_stmt(body, sc);
        },
        Stmt::Switch { expr, cases, .. } => {
            scan_expr(expr, sc);
            for c in cases {
                scan_block(&c.body, sc);
            }
        },
        Stmt::Label { stmt: inner, .. } => scan_stmt(inner, sc),
        Stmt::GotoIndirect { expr, .. } => scan_expr(expr, sc),
        Stmt::AsmGoto {
            outputs, inputs, ..
        } => {
            for o in outputs.iter().chain(inputs.iter()) {
                collect_asm_operand(o, &mut sc.asm_names);
            }
        },
        Stmt::Invoke { call, .. } => scan_stmt(call, sc),
        Stmt::EhLandingPad { object, selector, .. } => {
            scan_expr(object, sc);
            scan_expr(selector, sc);
        },
        Stmt::Goto { .. } | Stmt::Break { .. } | Stmt::Continue { .. }
        | Stmt::Fallthrough { .. } => {},
    }
}

/// 递归走完表达式。刻意不留宽松的 `_ => {}`：新增 `Expr` 变体会直接编不过，而不是
/// 静默漏收一个名字 —— 漏收的代价是「本该被否决的变量被绑进寄存器」，那是静默错值。
fn scan_expr(e: &Expr, sc: &mut Scan) {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::ComplexLit { .. }
        | Expr::ComplexIntLit { .. }
        | Expr::String { .. }
        | Expr::Char { .. }
        | Expr::SizeofTy { .. }
        | Expr::AlignofTy { .. }
        | Expr::Offsetof { .. }
        | Expr::LabelAddr { .. }
        | Expr::Ident { .. } => {},
        Expr::Unary { e: x, .. }
        | Expr::Cast { e: x, .. }
        | Expr::AddrOf { e: x, .. }
        | Expr::Deref { e: x, .. }
        | Expr::Real { e: x, .. }
        | Expr::Imag { e: x, .. }
        | Expr::SizeofExpr { e: x, .. }
        | Expr::AlignofExpr { e: x, .. }
        | Expr::PostInc { e: x, .. }
        | Expr::PostDec { e: x, .. }
        | Expr::PreInc { e: x, .. }
        | Expr::PreDec { e: x, .. } => scan_expr(x, sc),
        Expr::Member { expr: x, .. } => scan_expr(x, sc),
        Expr::Binop { l, r, .. }
        | Expr::Assign { l, r, .. }
        | Expr::Comma { l, r, .. }
        | Expr::Index { arr: l, idx: r, .. } => {
            scan_expr(l, sc);
            scan_expr(r, sc);
        },
        Expr::Cond {
            cond, then_, else_, ..
        } => {
            scan_expr(cond, sc);
            scan_expr(then_, sc);
            scan_expr(else_, sc);
        },
        Expr::Call { callee, args, .. } => {
            scan_expr(callee, sc);
            for a in args {
                scan_expr(a, sc);
            }
        },
        // 语句表达式里的声明属于内层作用域：它与外层同名而类型不同就是遮蔽。
        Expr::StmtExpr { block, .. } => scan_block(block, sc),
        Expr::CompoundLit { inits, .. } | Expr::InitList { inits, .. } => {
            for it in inits {
                match it {
                    InitItem::Expr(x) => scan_expr(x, sc),
                    InitItem::Designated { value, .. } => scan_expr(value, sc),
                }
            }
        },
        Expr::VaArg { ap, .. } => scan_expr(ap, sc),
        Expr::AsmExpr {
            outputs, inputs, ..
        } => {
            for o in outputs.iter().chain(inputs.iter()) {
                collect_asm_operand(o, &mut sc.asm_names);
            }
        },
    }
}

/// 一个 asm 操作数表达式的根对象名（`x`、`x[i]`、`p->f`、`(*p).f` 都算 `x`/`p`）。
fn collect_asm_operand(e: &Expr, out: &mut HashSet<String>) {
    if let Some(root) = ident_root_name(e) {
        out.insert(root);
    }
}

fn ident_root_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident { name, .. } => Some(name.name.clone()),
        Expr::Member { expr, .. } | Expr::Cast { e: expr, .. } => ident_root_name(expr),
        Expr::Index { arr, .. } => ident_root_name(arr),
        Expr::Deref { e, .. } => ident_root_name(e),
        _ => None,
    }
}

/// 把分配结果写进 AST。只碰 `register_name` 仍为空的声明（用户写死的优先）。
///
/// **Phase 3.2 的形状**：输入是 `plan.loc`（位置表）而不是 `plan.bindings`。今天两者必然
/// 一致（表就是 `to_loc_map(bindings)` 建的），所以产物逐字节不变是这一步的判据；而
/// 从这一步起，「让 AST 看到分配结果」这条路只剩一个入口。
///
/// **Phase 4.1b**：同一次写入也填 `VarDecl::home`（已按通道解释好的号）。两条同时存在不是
/// 双真源：`register_name` 是用户文本名的载体（本函数不写用户声明，只填自动分配的缺口），
/// `home` 是分配结果；两者在 `the_ast_landing_comes_from_the_table` 里被要成一致。
/// Phase 4.1c/d 会让后端改读 `home` 并拆掉 `register_name` 这条中间通道。
fn apply_bindings(b: &mut Block, plan: &mut Plan, splits: &Splits) {
    // 表 → 「AST 局部名 → 家」。不可表示的家（非 GP 通道、池外号，以及分裂也没能
    // 消除的同基名冲突）走下面的拒因点名，不默默绑。
    //
    // 以前这里只放行 `Reg`，其余一律当「不可表示」丢弃 —— 那就是 §11.13 给 5.1.e 点名的
    // 表达缺口：`Spill` 从 Phase 5.1.c 起已是后端**已接线**的家（读/写/取地址三个消费臂
    // 全部走 `LayoutMap::spill_off`，带的尺寸也从交付的家导出），唯一断的就是这里不交付。
    // 今天没有任何生产者给出 `Spill`（下面的 `bind` 只发 `Reg`），所以放行它是零产物影响的；
    // 不放行，则分配器永远拿不到「把这个值存到溢出槽」这一项能力。
    let mut homes: HashMap<String, Location> = HashMap::new();
    let mut unrepresentable: Vec<String> = Vec::new();
    for (local, loc) in plan.loc.fold_for_codegen(splits) {
        match loc {
            Some(Location::Reg(r)) if r.bank == RegBank::Gp && pool_reg_name(r.n).is_some() => {
                homes.insert(local, Location::Reg(r));
            }
            Some(Location::Spill(slot)) => {
                // 溢出槽就是内存：它不需要名字表里的文本名，也不占用任何寄存器
                homes.insert(local, Location::Spill(slot));
            }
            Some(_) => unrepresentable.push(local),
            None => unrepresentable.push(local),
        }
    }
    unrepresentable.sort();
    for local in unrepresentable {
        plan.rejected.push((local, "home-not-representable"));
    }
    // `Option<&'static str>` 就是「这个家要不要同时写 `register_name`」：寄存器家要
    // （旧代码靠它作为同源的名单），spill 家不要（它不是寄存器，写了就是骗后端去保存）。
    let map: HashMap<&str, (Option<&'static str>, Location)> = homes
        .iter()
        .map(|(k, loc)| {
            let nm = match loc {
                Location::Reg(r) => pool_reg_name(r.n),
                _ => None,
            };
            (k.as_str(), (nm, *loc))
        })
        .collect();
    fn stmt(s: &mut Stmt, map: &HashMap<&str, (Option<&'static str>, Location)>) {
        match s {
            Stmt::Block { b, .. } => walk(b, map),
            Stmt::If { then_, else_, .. } => {
                stmt(then_, map);
                if let Some(e) = else_ {
                    stmt(e, map);
                }
            },
            Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt(body, map),
            Stmt::For {
                init, body, ..
            } => {
                if let Some(i) = init {
                    stmt(i, map);
                }
                stmt(body, map);
            },
            Stmt::Switch { cases, .. } => {
                for c in cases {
                    walk(&mut c.body, map);
                }
            },
            Stmt::Label { stmt: inner, .. } => stmt(inner, map),
            Stmt::Invoke { call, .. } => stmt(call, map),
            _ => {},
        }
    }
    fn walk(b: &mut Block, map: &HashMap<&str, (Option<&'static str>, Location)>) {
        for item in &mut b.items {
            match item {
                BlockItem::Decl(Decl::Var(v)) => {
                    if let Some((rn, loc)) = map.get(v.name.name.as_str()) {
                        if v.register_name.is_none() {
                            if let Some(name) = rn {
                                v.register_name = Some((*name).to_string());
                            }
                        }
                        // Phase 4.1b：家同时交出去（位与名字同源，不会出现一边有一边没）。
                        v.home = Some(*loc);
                    }
                },
                BlockItem::Stmt(s) => stmt(s, map),
                _ => {},
            }
        }
    }
    walk(b, &map);
}

// ---------------------------------------------------------------------------
// IR 侧的基名活性区间
// ---------------------------------------------------------------------------

struct Span {
    first: i32,
    last: i32,
    /// 最早那次出现是定义，**且**这个定义落在 entry 块里。
    ///
    /// 判据是「最早那次出现是定义，**且该定义属于 entry 块**」。
    ///
    /// 为什么要带上块身份（实测踩过）：`long s(int c){long x;if(c)x=1;else x=2;return x;}`
    /// 里 `x` 的区间是 `[1,5]`，而 entry 块的指令数也刚好让「位置 ≤ entry 末尾」成立 ——
    /// 用位置数值当界会差一个边界就把分支里的定义算进 entry，把「只在某条路径初始化」
    /// 的变量绑走；那条未初始化路径读到的就是**调用者留在寄存器里的值**（-O0 读到栈
    /// 垃圾），同一个未定义行为在两个档位各给一个确定值。块身份没有这个歧义。
    entry_def: bool,
    defs: usize,
    uses: usize,
}

/// IR 里「一个 SSA 名出现了一次」的**唯一**遍历。
///
/// `base_spans`（活性区间）与 `to_loc_map`（版本粒度的家）必须走同一次遍历：
/// 「哪些出现算数」不允许有两份答案 —— 一边看到了、另一边漏了，就是「家挂在
/// 一个没人读的名字上」或「区间比实际短」那一族静默错值。参数与旧 `base_spans`
/// 里的计数口径逐字一致（`pos` 按 `block_order` 对 phi 槽/指令/terminator 递增，
/// `is_def`、`in_entry` 同旧义）。
fn each_occurrence(func: &Function, mut visit: impl FnMut(&SsaVar, i32, bool, bool)) {
    let mut pos: i32 = 0;
    for bname in &func.block_order {
        let Some(bb) = func.blocks.get(bname) else {
            continue;
        };
        let in_entry = bname == &func.entry;
        for phi in &bb.phis {
            // phi 的 dest 是本块的定义，incoming 是前驱末尾的使用：位置若相同，
            // 必须记成「最早出现是使用」，否则会把「读旧值」当成已初始化。
            visit(&phi.dest, pos, true, in_entry);
            for (src, _) in &phi.incoming {
                visit(src, pos, false, false);
            }
            pos += 1;
        }
        for inst in &bb.insts {
            for op in crate::ssa::kind_operand_ssa_vars(&inst.kind) {
                visit(&op, pos, false, in_entry);
            }
            if let Some(d) = &inst.dest {
                visit(d, pos, true, in_entry);
            }
            pos += 1;
        }
        for v in crate::ssa::ir::term_operand_ssa_vars(&bb.term) {
            visit(&v, pos, false, false);
        }
        pos += 1;
    }
}

/// 每个基名在 IR 里的 `[最早, 最晚]` 出现位置与定义/使用计数。
///
/// 位置按 `block_order` 顺序对 phi 槽、指令、terminator 递增编号（遍历本身在
/// `each_occurrence`，与位置表共用）。用途有两个：判「第一次出现是不是 entry 块里的
/// 定义」，以及给候选者排序（访问次数、区间起点）。
/// **不**用来判断两个变量能否共用一个号 —— 那件事在这个后端没有意义，见 `plan_for`。
fn base_spans(func: &Function) -> HashMap<String, Span> {
    let mut out: HashMap<String, Span> = HashMap::new();
    each_occurrence(func, |var, pos, is_def, in_entry| {
        note(&mut out, &var.base, pos, is_def, in_entry)
    });
    out
}

fn note(m: &mut HashMap<String, Span>, name: &str, at: i32, is_def: bool, in_entry: bool) {
    if let Some(e) = m.get_mut(name) {
        if at < e.first {
            e.first = at;
            e.entry_def = is_def && in_entry;
        } else if at == e.first {
            // 同一位置上既有定义又有使用 ⇒ 不能算「先定义」
            e.entry_def &= is_def && in_entry;
        }
        if at > e.last {
            e.last = at;
        }
        if is_def {
            e.defs += 1;
        } else {
            e.uses += 1;
        }
        return;
    }
    m.insert(
        name.to_string(),
        Span {
            first: at,
            last: at,
            defs: if is_def { 1 } else { 0 },
            uses: if is_def { 0 } else { 1 },
            entry_def: is_def && in_entry,
        },
    );
}

/// 把 P12-A 的分配结果（基名粒度）落成 IR 侧的**版本粒度**位置表。
///
/// 规则只有一条：某基名被绑到 `%rN` ⇒ 它在 IR 里的**每一个** SSA 版本的家都是那个号。
/// 这不是偷懒，而是今天后端的表示能力（绑定表是函数级 `名字 → 号`，读写每次查同一张表，
/// 中途没有换家的时机 —— 见 §7.20 那条「一个号只给一个变量」的实测判据）。所以本函数
/// 产出表与 `LocMap::fold_by_base` 折叠回去必然一致；Phase 5 的分配器若开始给不同版本
/// 分不同号，折叠会点名（`None`）而不是默默取一个。
///
/// 与 `base_spans` 共用 `each_occurrence`：「哪些出现算数」不允许有两份答案。
pub fn to_loc_map(plan: &Plan, func: &Function) -> LocMap {
    let mut out = LocMap::empty();
    // 基名 → 自动池里的号。绑到自动池外的号才不进表：`auto_reg_of` 就是那条判据的唯一出处。
    let wanted: HashMap<&str, u8> = plan
        .bindings
        .iter()
        .filter_map(|(base, rn)| auto_reg_of(rn).map(|n| (base.as_str(), n)))
        .collect();
    if wanted.is_empty() {
        return out;
    }
    each_occurrence(func, |var, _, _, _| {
        if let Some(&n) = wanted.get(var.base.as_str()) {
            out.bind(var, Location::Reg(RegId { bank: RegBank::Gp, n }));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 走完「AST → SSA → 优化 → 重建 → 分配」，与 `opt::run_ssa_passes` 的接线一致。
    ///
    /// 四个返回值都要：`Function` 用来独立复核活性区间（不能只信分配器的话），
    /// 重建前的体是判据输入、重建后的体用来验证结果真的写进了 AST，`Plan` 是被测对象。
    fn run_pipeline(
        src: &str,
        fname: &str,
        level: u8,
        ctx: &Ctx,
    ) -> (Function, Block, Block, Plan) {
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let f = m
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                crate::ast::Decl::Func(f) if f.name.name == fname => Some(f.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("源里没有函数 {fname}"));
        let mut ssa = crate::ssa::build_from_func(&f).unwrap_or_else(|| {
            panic!("{fname} 应能 SSA 化（否则这条测试测不到分配）")
        });
        crate::ssa::passes::run_with_opts(&mut ssa, level, true);
        let orig = f.body.clone().expect("被测函数必须有体");
        let mut body = crate::ssa::rebuild_block(&ssa);
        let plan = assign(&mut ssa, &orig, &mut body, ctx);
        (ssa, orig, body, plan)
    }

    fn ctx_default() -> Ctx {
        Ctx { debug_info: false, taken_regs: HashSet::new() }
    }

    fn bound_names(plan: &Plan) -> Vec<String> {
        plan.bindings.iter().map(|(n, _)| n.clone()).collect()
    }

    fn reg_of<'a>(plan: &'a Plan, name: &str) -> Option<&'a str> {
        plan.bindings
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| *r)
    }

    fn reason_of<'a>(plan: &'a Plan, name: &str) -> Option<&'a str> {
        plan.rejected
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, w)| *w)
    }

    /// D5 的守卫现在只到「拦」为止：Phase 6.2 给 DWARF 接上 `DW_OP_regN` 后，`-g`
    /// 不再拦寄存器家。本用例钉的是**翻过来的那一面**：`-g` 下照样绑，但准入判据照跑。
    ///
    /// 为什么不删掉这个用例：它是「`-g` 与产物形状」这条关系上唯一直接可读到的一条。
    /// 删了就只能从三档哈希间接推，而哈希本身分不清「没绑」与「绑了但读不到」。
    #[test]
    fn debug_info_no_longer_gates_the_register_home() {
        let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}\n";
        let gctx = Ctx { debug_info: true, taken_regs: HashSet::new() };
        let (_ssa, _orig, _body, plan) = run_pipeline(src, "s", 2, &gctx);
        assert!(
            !plan.bindings.is_empty(),
            "-g 下应照样绑得上（Phase 6.2 已接线 DW_OP_regN，实际 {:?}）",
            plan.bindings
        );
        for w in ["debug-info-forbids-register-home", "register-home-disallowed-by-location-policy"] {
            assert!(
                !plan.rejected.iter().any(|(_, r)| *r == w),
                "旧拒因 {w} 不得再出现在 -g 的拒因里，实际 {:?}",
                plan.rejected
            );
        }
        // 反向对照：取消拦的是「能不能拿寄存器家」，不是准入判据。被取址的局部仍然
        // 必须不绑（否则本用例会退化成「-g 下逢人就绑」）。
        let esc = "long s(long n){long k=n;long*p=&k;*p+=n;return *p;}\n";
        let (_s2, _o2, _b2, p2) = run_pipeline(esc, "s", 2, &gctx);
        assert!(
            p2.rejected.iter().any(|(_, w)| *w == "address-escaped"),
            "-g 下取过址的局部仍必须被准入判据拒掉，实际 {:?}",
            p2.rejected
        );
    }

    /// 非空验证：上面那个全语料∀ 口径量出的「池满拒绑 148 个，可共用 0 个」既可能是
    /// 真结论，也可能是度量算错。拿一个结构上明确「最后一个变量与前面都不重叠」的源跑：
    /// 如果连它都报 0，那度量是坏了，而不是没机会。
    #[test]
    fn shareable_metric_is_not_vacuously_zero() {
        // a..e 依次短命：每个只在自己那条语句里活。必须依赖形参：常量会被 SCCP 折叠，
        // 连 IR _presence 都没有（那就是 `fully_folded_locals_have_no_ir_presence` 记的现象），
        // 本用例就会退化成空跑 —— 第一版就是这么错的。
        // 候选数要**多于自动池**。延伸池目前不开（见 `auto_pool`），所以自动池就是
        // `POOL` 那 4 个 callee-saved；开池的那天本用例要跟着长到 10 个候选，而那
        // 一天必须先解决暂存约定（设计 §11.39）。
        let src = "long sink;\nlong f(long k){long a=k+1;sink=sink+a;long b=k+2;sink=sink+b;\
                   long c=k+3;sink=sink+c;long d=k+4;sink=sink+d;long e=k+5;sink=sink+e;return sink;}\n";
        let (ssa, _orig, _body, plan) = run_pipeline(src, "f", 2, &ctx_default());
        assert!(
            plan.bindings.len() >= 4,
            "被测源应先把自动池绑满（实得 {}），否则本用例是空跑",
            plan.bindings.len()
        );
        let pool_full = plan
            .rejected
            .iter()
            .filter(|(_, w)| *w == REASON_NO_FREE_REG)
            .count();
        assert!(
            pool_full >= 1,
            "自动池只有 4 个号（延伸池未开），5 个候选应至少拒一个，实际 rejected={:?}",
            plan.rejected
        );
        assert!(
            shareable_rejects(&ssa, &plan) >= 1,
            "依次短命的第 5 个变量本可复用已绑号；报 0 说明度量坏了（而不是没机会）"
        );
    }

    /// Phase 5.0 的核心不变式：决策前置不能漏掉后置事实。
    ///
    /// 旧形状把重建后的体也当决策输入；现在它只是校验。如果某个已绑基名只在重建后的
    /// 体里露出「不该绑」的事实，那前置就是错的假设，而且它会拖动帧布局 —— 所以这条
    /// 必须能在用例里直接断言（不靠 grep 诊断文本）。跑真实夹具（含内联汇编与地址逸出
    /// 两份专门夹具），不靠手写小段代码。
    #[test]
    fn post_rebuild_facts_do_not_change_the_plan() {
        const PATHS: [&str; 3] = [
            "tests/fixtures/asm/asm_register_binding_clobber.c",
            "tests/fixtures/asm/asm_register_binding_escape.c",
            "tests/fixtures/opt/ssa_regalloc_locals.c",
        ];
        let mut checked = 0usize;
        let mut functions_with_bindings = 0usize;
        for path in PATHS {
            let src = std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path),
            )
            .unwrap_or_else(|e| panic!("读不到 {path}：{e}（用例不得因为文件缺失而空跑）"));
            let (prog, _d) = crate::parser::parse(src.as_bytes(), path);
            let (m, _w) = crate::ir::lower(prog).expect("夹具应能 lower");
            for d in &m.source().decls {
                let f = match d {
                    crate::ast::Decl::Func(f) if f.body.is_some() => f,
                    _ => continue,
                };
                let Some(mut ssa) = crate::ssa::build_from_func(f) else { continue };
                crate::ssa::passes::run_with_opts(&mut ssa, 2, true);
                if crate::ssa::has_multi_exit_cycle(&ssa) {
                    continue;
                }
                let ctx = Ctx { debug_info: false, taken_regs: HashSet::new() };
                let mut plan = plan_for(&ssa, f.body.as_ref().unwrap(), &ctx);
                plan.loc = to_loc_map(&plan, &ssa);
                let rebuilt = crate::ssa::rebuild_block(&ssa);
                let conflicts = post_rebuild_conflicts(&plan, &rebuilt);
                assert!(
                    conflicts.is_empty(),
                    "{path} 的 {}：决策前置漏掉了后置事实 {:?}",
                    f.name.name, conflicts
                );
                checked += 1;
                if !plan.bindings.is_empty() {
                    functions_with_bindings += 1;
                }
            }
        }
        // 假绿防护：真的扫到了函数，而且至少有一个函数绑上了号（否则上面断言是空跑）
        assert!(checked >= 3, "只校验了 {checked} 个函数，用例覆盖面不足");
        assert!(
            functions_with_bindings >= 1,
            "所有被校函数都没绑任何号 ⇒ 后置校验断言根本没有判定力"
        );
    }

    /// Phase 3.2 的判据本体：AST 上的绑定**只能来自位置表**，且与诊断读数一致。
    ///
    /// 三个读数必须同构：表的基名折叠、写进 AST 的 `register_name`、`plan.bindings`。
    /// 今天它们来同一次分配，所以逐字相等是应有的；谁改了其中一个而没改其他，这条就红。
    /// （Phase 4 会去掉 AST 这条中间通道，那时本用例改成「表 ↔ 后端实际存取」。）
    #[test]
    fn the_ast_landing_comes_from_the_table() {
        let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}\n";
        let (ssa, _orig, body, plan) = run_pipeline(src, "s", 2, &ctx_default());

        // 表→基名→号
        let from_table: HashMap<String, u8> = plan
            .loc
            .fold_by_base()
            .into_iter()
            .filter_map(|(b, l)| match l {
                Some(Location::Reg(r)) => Some((b, r.n)),
                _ => None,
            })
            .collect();
        assert!(!from_table.is_empty(), "被测源应至少绑出一个变量");
        // 诊断读数必须与表一致（名字→号走 POOL，两者同一个号表）
        for (base, rn) in &plan.bindings {
            let n = pooled_reg_of(rn).expect("绑定名必须是池里的号");
            assert_eq!(from_table.get(base), Some(&n), "表与 bindings 对不上：{base}");
        }
        assert_eq!(from_table.len(), plan.bindings.len(), "表与 bindings 集合大小不等");
        // IR 里携带的必须是同一张表（assign 已把它写进 func.value_loc）
        assert_eq!(
            ssa.value_loc.sorted_entries(),
            plan.loc.sorted_entries(),
            "IR 上的位置表与分配结果不同步"
        );
        // AST 侧：每个带 register_name 的声明都对应表里的一个号，名字用 POOL 校
        let mut landed = 0;
        fn walk(b: &Block, table: &HashMap<String, u8>, n: &mut usize) {
            for item in &b.items {
                match item {
                    BlockItem::Decl(Decl::Var(v)) => {
                        if let Some(rn) = &v.register_name {
                            let got = pooled_reg_of(rn).expect("落地名必须是池里的号");
                            assert_eq!(
                                table.get(&v.name.name).copied(),
                                Some(got),
                                "AST 把 {} 绑到了 %{}，但表里没有这个家",
                                v.name.name,
                                rn
                            );
                            // Phase 4.1b：家与文本名必须同时存在且同号（后端即将只读前者）。
                            assert_eq!(
                                v.home,
                                Some(Location::Reg(RegId { bank: RegBank::Gp, n: got })),
                                "{} 的 home 与 register_name 不同源",
                                v.name.name
                            );
                            *n += 1;
                        } else {
                            // 两条通道不得漂：没有寄存器名就不能有**寄存器**家。而 `Spill`
                            // 是内存家，它本来就不该有 `register_name`（Phase 5.1.e 的交付形状）。
                            assert!(
                                v.home.is_none() || matches!(v.home, Some(Location::Spill(_))),
                                "{} 没有 register_name 却带了非内存的家（两条通道开始漂了）",
                                v.name.name
                            );
                        }
                    }
                    BlockItem::Stmt(s) => stmt_walk(s, table, n),
                    _ => {}
                }
            }
        }
        fn stmt_walk(s: &Stmt, table: &HashMap<String, u8>, n: &mut usize) {
            match s {
                Stmt::Block { b, .. } => walk(b, table, n),
                Stmt::If { then_, else_, .. } => {
                    walk_then(then_, table, n);
                    if let Some(e) = else_ {
                        walk_then(e, table, n);
                    }
                }
                Stmt::While { body, .. } | Stmt::Do { body, .. } => stmt_walk(body, table, n),
                Stmt::For { body, .. } => stmt_walk(body, table, n),
                Stmt::Switch { cases, .. } => {
                    for c in cases {
                        walk(&c.body, table, n);
                    }
                }
                _ => {}
            }
        }
        fn walk_then(s: &Stmt, table: &HashMap<String, u8>, n: &mut usize) {
            stmt_walk(s, table, n);
        }
        walk(&body, &from_table, &mut landed);
        assert_eq!(landed, from_table.len(), "表里每个家都应在 AST 上有一个落地声明");
    }

    /// Phase 3.1 的等价性判据：位置表与绑定结果**必须一致**（本阶段仍无人读它）。
    ///
    /// 为什么现在就钉：Phase 4 把消费方从 `register_name` 切到 `value_loc` 时，如果表与
    /// 绑定结果不一致，错会体现为产物变化（难归因）；而在 3.1 只需一条单测就能红。
    #[test]
    fn to_loc_map_agrees_with_the_plan_at_base_granularity() {
        let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}\n";
        let (ssa, _orig, _body, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(
            plan.bindings.len() >= 2,
            "被测源应至少绑两个变量，实际 {:?}",
            plan.bindings
        );

        let loc = to_loc_map(&plan, &ssa);

        // (a) 折叠回基名 == plan.bindings（同集合、同号）
        let mut got = loc.fold_by_base();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        let mut want: Vec<(String, Option<Location>)> = plan
            .bindings
            .iter()
            .map(|(b, rn)| {
                let n = pooled_reg_of(rn).expect("绑定名必须是池里的号");
                (b.clone(), Some(Location::Reg(RegId { bank: RegBank::Gp, n })))
            })
            .collect();
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(got, want, "位置表折叠回基名必须与绑定结果一一对应");

        // (b) 覆盖性：表恰好覆盖被绑基名的每一个 SSA 版本，不多不少、无悬空键
        let bound: HashSet<String> = plan.bindings.iter().map(|(b, _)| b.clone()).collect();
        let mut all_names: HashSet<String> = HashSet::new();
        let mut want_keys: HashSet<String> = HashSet::new();
        each_occurrence(&ssa, |v, _, _, _| {
            all_names.insert(v.name());
            if bound.contains(&v.base) {
                want_keys.insert(v.name());
            }
        });
        let have: HashSet<String> = loc.sorted_entries().into_iter().map(|(k, ..)| k).collect();
        assert_eq!(have, want_keys, "表的键集合必须等于被绑基名的全部 SSA 版本");
        assert!(
            loc.dangling_keys(|k| all_names.contains(k)).is_empty(),
            "表里不得有在 IR 找不到出现的键"
        );
    }

    /// 热循环里的累加器与归纳变量都该住进寄存器，而且**必须真的落到 AST 上**
    /// （重建后的 `Decl::Var` 带上了 `register_name`），否则后端根本看不到分配结果。
    #[test]
    fn hot_loop_locals_move_into_registers_and_into_the_ast() {
        let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}\n";
        let (_, _, body, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(
            bound_names(&plan).contains(&"acc".to_string())
                && bound_names(&plan).contains(&"i".to_string()),
            "累加器与归纳变量都应被绑，实际绑定 {:?}、拒绝 {:?}",
            plan.bindings,
            plan.rejected
        );
        assert_ne!(
            reg_of(&plan, "acc"),
            reg_of(&plan, "i"),
            "同时活着的两个变量不得共用一个寄存器"
        );
        // AST 侧必须看得到
        let mut seen = 0;
        for item in &body.items {
            if let BlockItem::Decl(crate::ast::Decl::Var(v)) = item {
                if v.register_name.is_some() {
                    seen += 1;
                }
            }
        }
        assert!(
            seen >= 2,
            "分配结果必须写进重建后的 AST（带 register_name 的声明数 = {seen}）"
        );
    }

    /// 同一个名字连续跑两次必须给出**逐字相同**的结果：候选集来自 HashMap，
    /// 只要排序里漏一个键，产物就会随哈希种子漂移（可复现性门禁会红）。
    #[test]
    fn plan_is_deterministic() {
        let src = "long s(long n){long a=1;long b=2;long c=n+3;return a*2+b*4+c*8;}\n";
        let (_, _, _, p1) = run_pipeline(src, "s", 2, &ctx_default());
        for _ in 0..8 {
            let (_, _, _, p2) = run_pipeline(src, "s", 2, &ctx_default());
            assert_eq!(p1.bindings, p2.bindings, "绑定结果不稳定");
            assert_eq!(p1.rejected, p2.rejected, "拒绝名单不稳定");
        }
    }

    /// 地址被取走过的变量一律不绑（后端会把它退回内存，绑了就是旧值）。
    #[test]
    fn address_escaped_variable_is_not_bound() {
        let src = "long s(long n){long a=0;long*p=&a;for(long i=0;i<n;i++)a+=i;return *p;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert_eq!(
            reg_of(&plan, "a"),
            None,
            "被取址的变量绝不能绑：它必须留在内存里，实际 {:?}",
            plan.bindings
        );
        assert_eq!(reason_of(&plan, "a"), Some("address-escaped"));
    }

    /// 出现在内联汇编操作数里的变量一律不绑：`"+r"`/`"=m"` 与「栈槽不再被写」
    /// 的组合还缺一整批实测，不交给自动分配。
    #[test]
    fn inline_asm_operand_is_not_bound() {
        let src = "long s(long n){long out=0;__asm__ volatile(\"addq $1, %0\" : \"+r\"(out));out+=n;return out;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert_eq!(reason_of(&plan, "out"), Some("inline-asm-operand"));
    }

    /// 类型/存储期/限定符各挡一条：volatile、static、浮点、数组、结构体、_Atomic。
    #[test]
    fn wrong_kind_variables_are_not_bound() {
        let src = "long s(long n){volatile long v=1;static long st=2;double d=3.0;long arr[2]={4,5};struct P{long x;} p={6};_Atomic long at=7;return v+st+(long)d+arr[0]+p.x+at+n;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        // 每个都断言两条：既没被绑，又有一个**具体**的拒绝原因（原因可能是
        // 「类型不行 / 不是普通局部 / IR 里没它」，都成立即安全）。
        for name in ["v", "st", "d", "arr", "p", "at"] {
            assert_eq!(
                reg_of(&plan, name),
                None,
                "{name} 不该被绑，实际 {:?}",
                plan.bindings
            );
            assert!(
                reason_of(&plan, name).is_some(),
                "{name} 应有一个可诊断的拒绝原因（dump 里看得见才叫可诊断）"
            );
        }
    }

    /// 一个号只发给一个变量 —— 后端没有「中途换家」的能力。
    ///
    /// 这条是被实测打脸后收紧的。第一版按「IR 区间不相交就可以共用号」实现，
    /// `tests/fixtures/c/char16_char32.c` 立刻错值（-O2 rc=3，gcc 与 -O0 是 0）：
    /// 两个变量的比较被 SCCP 折掉之后，它们在 IR 里的区间退化成定义那一个点，
    /// 于是被判成互不相交而共用 %rbx —— 可绑定表是**函数级**的 `名字 -> 号`，
    /// 两个名字同号就是同一个家被两个人写，跟区间重叠与否无关。
    ///
    /// 形状换成「循环里三个各自被折掉一部分使用的累加器」：这条会真的绑到 >= 2 个
    /// 变量（`tests::only_folded_locals_have_no_ir_presence` 记录的是另一半事实 ——
    /// 全被折掉的名字连候选者都不是，所以那条不能拿来测复用）。
    #[test]
    fn each_register_goes_to_exactly_one_variable() {
        let src = "long s(long n){long a=0;long b=1;long c=2;for(long i=0;i<n;i++){a+=i;b+=a;c+=b;}return a+b+c;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(
            plan.bindings.len() >= 2,
            "这条样例本就该同时绑到两个变量（否则本测试是空判）：绑定 {:?}，拒绝 {:?}",
            plan.bindings,
            plan.rejected
        );
        let regs: Vec<&str> = plan.bindings.iter().map(|(_, r)| *r).collect();
        let uniq: std::collections::HashSet<&str> = regs.iter().copied().collect();
        assert_eq!(
            regs.len(),
            uniq.len(),
            "同一个号被发给了两个变量（后端没有中途换家的能力）：{:?}",
            plan.bindings
        );
        assert!(
            regs.len() <= POOL.len(),
            "绑定的变量数不可能超过池子大小：{:?}",
            plan.bindings
        );
    }

    /// Phase 5.2a 的分工：**守得住的 clobber 不在这里让号**。
    ///
    /// clobber 说的是「本条 asm 之后这个号的内容不保证」。普通 `asm volatile` 没有跳出
    /// 目标，后端在发射点前后各插一条保存/恢复就守得住（`asm/emit.rs::asm_guard_regs`；
    /// 产物形态见夹具 `tests/fixtures/asm/asm_clobber_home_and_temp.c`），所以分配器不得
    /// 因为这种 clobber 少绑一个变量 —— 那等于把 P12 的收益在最常见形状上缴掉。
    /// 这条是反向对照：守得住时，同一个号必须仍然被绑出去。
    #[test]
    fn guardable_asm_clobber_does_not_shrink_the_pool() {
        let plain =
            "long s(long n){long a=0;long b=1;long c=2;for(long i=0;i<n;i++){a+=i;b+=a;c+=b;}return a+b+c;}\n";
        let with_asm = "long s(long n){long a=0;long b=1;long c=2;for(long i=0;i<n;i++){\
                        a+=i;__asm__ volatile(\"movq $1, %%rbx\" ::: \"rbx\");b+=a;c+=b;}\
                        return a+b+c;}\n";
        let (_f0, _o0, _b0, p0) = run_pipeline(plain, "s", 2, &ctx_default());
        let (_f1, _o1, _b1, p1) = run_pipeline(with_asm, "s", 2, &ctx_default());
        assert!(
            p0.bindings.iter().any(|(_, r)| *r == "rbx"),
            "基线源里应有人拿到 %rbx（否则本用例是空跑）：{:?}",
            p0.bindings
        );
        assert!(
            p1.bindings.iter().any(|(_, r)| *r == "rbx"),
            "%rbx 被一条守得住的 asm clobber 了，分配器不得因此让号：{:?}",
            p1.bindings
        );
    }

    /// 为什么分配器**不需要**为 `asm goto` 的 clobber 另外建一份让号判据：这类函数今天
    /// 根本进不了 SSA 管线 —— `is_structured` 看见 `Label`/`AsmGoto` 就整函数降级回 AST
    /// 路径（`ssa::build::build_from_func` 在那一处返 `None`），于是一个号也不会被自动绑走。
    ///
    /// 本用例钉住的是这条**前提**，不是「已经处理了」。前提一变（比如将来允许 label 进
    /// SSA），这里就会红，提醒把判据补回 `plan_for`：事实已经由
    /// `crate::ast_asm::escaping_asm_clobbers` 收好，后端那一侧（`ban_escaping_asm_regs`）
    /// 也已经在读同一个判据，所以补回来的位置只有一个。
    #[test]
    fn escaping_asm_functions_do_not_reach_the_allocator() {
        let func_of = |src: &str| {
            let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
            let (m, _w) = crate::ir::lower(prog).unwrap();
            m.source()
                .decls
                .iter()
                .find_map(|d| match d {
                    crate::ast::Decl::Func(f) if f.name.name == "s" => Some(f.clone()),
                    _ => None,
                })
                .expect("源里有函数 s")
        };
        // 逃逸形态：asm goto 带着跳出目标，而目标标签就在同一个函数里
        let escape = "long s(long n){long a=0;for(long i=0;i<n;i++){a+=i;\
                      __asm__ goto(\"jmp %l0\" ::: \"rbx\" : skip);}\
                      a+=1;skip:return a;}\n";
        let fd = func_of(escape);
        assert!(
            crate::ssa::build_from_func(&fd).is_none(),
            "带逃逸 asm goto 的函数必须不进 SSA（否则变量的家可能被绑到一个毁得住的号上）"
        );
        // 反向对照：同一个号被**普通** asm clobber 时函数照进 SSA（上面那条用例要求它绑得上）
        let guardable = "long s(long n){long a=0;for(long i=0;i<n;i++){\
                         __asm__ volatile(\"\" ::: \"rbx\");a+=i;}return a;}\n";
        let fd2 = func_of(guardable);
        assert!(
            crate::ssa::build_from_func(&fd2).is_some(),
            "普通 asm 不该把函数挡在 SSA 外面（那会让上面那条反向对照变成空跑）"
        );
    }

    /// 被常量折叠彻底拿掉使用的局部，在 IR 里连一次出现都没有 ⇒ 一律不绑。
    ///
    /// 这条不是「保守得没意义」，而是把上一条测试说不到的那一半说清楚：
    /// `long a=65; if(a!=65) return 3;` 经过 SCCP 之后在 IR 里**没有任何**痕迹，
    /// 于是它连候选者都不是（`no-ir-presence`）。第一版正是在这种名字上判成
    /// 「区间退化成点 ⇒ 与别人不相交」而共用了 %rbx。
    #[test]
    fn fully_folded_locals_have_no_ir_presence() {
        let src = "long s(void){long a=65;long b=66;if(a!=65)return 3;if(b!=66)return 4;return 0;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(plan.bindings.is_empty(), "全被折掉的名字不该被绑：{:?}", plan.bindings);
        assert_eq!(reason_of(&plan, "a"), Some("no-ir-presence"));
        assert_eq!(reason_of(&plan, "b"), Some("no-ir-presence"));
    }

    /// 形参不绑：它在 IR 里的第一次出现是使用（值由 ABI 送进来），绑上就等于把
    /// 调用者留在寄存器里的值当成初始值。
    #[test]
    fn parameter_is_not_bound() {
        let src = "long s(long n){long m=n*2;return m+1;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        // 形参不在 `Decl::Var` 里，所以它连候选者都不是：断言「没被绑」这条性质。
        assert_eq!(reg_of(&plan, "n"), None, "形参绝不能绑：它的值由 ABI 送进来");
        assert_eq!(reason_of(&plan, "m"), None, "函数里定义的 m 应当可以被绑");
    }

    /// 「只在某条路径上赋值」的变量一律不绑。这一条守的是等价性门禁：不这么限
    /// 的话，未初始化那条路径在 -O0 读到栈垃圾、在 -O2 读到**调用者留下的寄存器值**，
    /// 同一个未定义行为给出两个确定值。
    #[test]
    fn conditionally_initialized_variable_is_not_bound() {
        let src = "long s(int c){long x;if(c)x=1;else x=2;return x;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert_eq!(
            reason_of(&plan, "x"),
            Some("not-defined-in-entry-block"),
            "绑定表 {:?}",
            plan.bindings
        );
    }

    /// 有嵌套函数的函数整体不绑：嵌套函数按帧偏移访问外层变量，而绑定变量的那个
    /// 偏移没人写。
    #[test]
    fn function_with_nested_function_is_untouched() {
        let src = "long s(long n){long acc=n;long inner(void){return acc;}return inner()+acc;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(
            plan.bindings.is_empty(),
            "含嵌套函数的整条 pass 都不该绑，实际 {:?}",
            plan.bindings
        );
    }

    /// 用户手写的 `register asm` 优先，且它占的号不能被自动分配再发一次。
    #[test]
    fn user_binding_wins_and_its_register_is_reserved() {
        let src = "long s(long n){register long v asm(\"rbx\");v=n+1;long w=v*2;long z=w+n;return z+v;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert_eq!(reason_of(&plan, "v"), Some("already-register-asm"));
        assert_ne!(
            reg_of(&plan, "w"),
            Some("rbx"),
            "%rbx 已被用户手写的绑定占住，自动分配不能再发给 w：{:?}",
            plan.bindings
        );
        assert_ne!(
            reg_of(&plan, "z"),
            Some("rbx"),
            "%rbx 已被用户占住，任何自动分配都不能用它：{:?}",
            plan.bindings
        );
    }

    /// file-scope 的 `register asm` 占住的号同样要让开（后端每个函数都会重新灌进表）。
    #[test]
    fn file_scope_taken_register_is_not_reused() {
        let src = "long s(long n){long a=n+1;long b=a*2;long c=b+3;long d=c*4;long e=d+5;long f2=e*6;return f2+d;}\n";
        let mut ctx = ctx_default();
        ctx.taken_regs.insert(3u8); // %rbx
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx);
        for (n, r) in &plan.bindings {
            assert_ne!(*r, "rbx", "{n} 不得使用被 file-scope 占住的 %rbx");
        }
    }

    /// `-g` 不再参与决定谁绑：同一个源在两档下的**计划必须逐项相等**。
    ///
    /// 这比「两档都绑得上」强：它同时钉住了「`-g` 不会少绑」与「`-g` 不会多绑」。
    /// 旧形状下这个断言会是 `[] == [(acc, rbx), (i, r12)]` 而红 —— 那条限制已经随
    /// Phase 6.2 接线 DW_OP_regN 删掉了（见 `register_homes_allowed`）。
    #[test]
    fn debug_info_does_not_change_the_plan() {
        let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}\n";
        let mut gctx = ctx_default();
        gctx.debug_info = true;
        let (_, _, _, gplan) = run_pipeline(src, "s", 2, &gctx);
        let (_, _, _, nplan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(
            !gplan.bindings.is_empty(),
            "空对空也能让下面的相等成立，所以先要求非空"
        );
        assert_eq!(
            gplan.bindings, nplan.bindings,
            "-g 与非 -g 的绑定表必须逐项相等（号也一样，否则 DWARF 与机器码会错位）"
        );
        assert_eq!(
            gplan.rejected, nplan.rejected,
            "-g 与非 -g 的拒因表也必须逐项相等"
        );
    }

    /// `DCC_OPT_SKIP=ssa_regalloc` 关掉的是调用点，这里验证 pass 自己也能整体退回：
    /// 不含可绑变量的函数必须一条都不报（防止把「没得绑」实现成「绑了但后端忽略」）。
    #[test]
    fn nothing_bindable_yields_empty_plan() {
        let src = "long s(long n){return n*2+1;}\n";
        let (_, _, _, plan) = run_pipeline(src, "s", 2, &ctx_default());
        assert!(plan.bindings.is_empty(), "{:?}", plan.bindings);
    }

    /// Phase 5.1.e 的**交付半边**：分配器一旦给出 `Spill` 家，它必须能一路走到 AST 声明上。
    ///
    /// 以前 `apply_bindings` 只放行 `Reg`，其余一律报 `home-not-representable` 丢弃 ——
    /// 而 `Spill` 从 Phase 5.1.c 起已是后端**已接线**的家（读/写/取地址三个消费臂全走
    /// `LayoutMap::spill_off`，带的尺寸也从交付的家导出）。那不交付，生产侧就无从表达；
    /// 今天没有任何分配结果给出 Spill，所以只能像带尺寸那条用例一样注入验机制本身。
    #[test]
    fn spill_homes_are_delivered_to_the_ast() {
        let src = "long s(long n){long acc=0;for(long i=0;i<n;i++)acc+=i;return acc;}\n";
        // 在**重建前**的那份体上交付：拿 `run_pipeline` 已经交付过的体再做一次会被
        // 「已有 register_name 不覆盖」那条旧规则遮住，测不到新臂的形状。
        let (_ssa, orig, _rebuilt, mut plan) = run_pipeline(src, "s", 2, &ctx_default());
        let mut body = orig.clone();
        // 注入：只有一个局部拿 `Spill(2)`，其余名字不在表上（它们必须既没家也没寄存器名）
        let mut m = LocMap::empty();
        m.bind(
            &crate::ssa::ir::SsaVar {
                base: "acc".to_string(),
                version: 1,
                ty: Type::Long,
                span: crate::diagnostics::Span::point("t.c", 1, 1),
            },
            Location::Spill(2),
        );
        plan.loc = m;
        apply_bindings(&mut body, &mut plan, &Splits::default());

        let mut found = 0;
        fn walk(b: &Block, found: &mut usize) {
            for item in &b.items {
                match item {
                    BlockItem::Decl(Decl::Var(v)) => {
                        if v.name.name == "acc" {
                            assert_eq!(
                                v.home,
                                Some(Location::Spill(2)),
                                "Spill 家必须原样交付到声明上"
                            );
                            assert!(
                                v.register_name.is_none(),
                                "内存家不得写 register_name（那会骗后端去保存一个不存在的寄存器）"
                            );
                            *found += 1;
                        } else {
                            assert_eq!(
                                v.home, None,
                                "不在表上的名字不得被顺手给家：{}",
                                v.name.name
                            );
                        }
                    },
                    // 递归下潜：`for` 的 init 里也有声明，只看顶层会把它们全遮住
                    BlockItem::Stmt(s) => walk_stmt(s, found),
                    _ => {},
                }
            }
        }
        fn walk_stmt(s: &Stmt, found: &mut usize) {
            match s {
                Stmt::Block { b, .. } => walk(b, found),
                Stmt::If { then_, else_, .. } => {
                    walk_stmt(then_, found);
                    if let Some(e) = else_ {
                        walk_stmt(e, found);
                    }
                },
                Stmt::While { body, .. } | Stmt::Do { body, .. } => walk_stmt(body, found),
                Stmt::For { init, body, .. } => {
                    if let Some(i) = init {
                        walk_stmt(i, found);
                    }
                    walk_stmt(body, found);
                },
                Stmt::Switch { cases, .. } => {
                    for c in cases {
                        walk(&c.body, found);
                    }
                },
                Stmt::Label { stmt, .. } => walk_stmt(stmt, found),
                Stmt::Invoke { call, .. } => walk_stmt(call, found),
                _ => {},
            }
        }
        walk(&body, &mut found);
        assert_eq!(found, 1, "本用例必须真的改到了一个叫 acc 的声明（否则只是空跑）");
        // 也不能顺手多一个拒因：`acc` 现在是可表示的家
        assert!(
            !plan.rejected.iter().any(|(n, _)| n == "acc"),
            "Spill 家不得被报成 home-not-representable：{:?}",
            plan.rejected
        );
    }
}
