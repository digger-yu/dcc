// src/ssa/location.rs —— SSA 值的**家**（Location）：P12 字面形态 Phase 3。
//
// 设计出处：`docs/p12-physreg-spill-design.md` §4 方案 B 与 §5 数据模型。一句话：
// 「谁住在哪个物理位置」必须是 IR 里的一张表，而不是 codegen 生成时现查的三套判据。
//
// 为什么挂在 `Function` 上而不是挂在 `ast::Ident` 上（方案 A 被否的三个理由，见 §4）：
//   * AST 在三后端之间共享，而寄存器号是后端事实 —— 写进前端契约就是影子语义；
//   * `Ident` 只能表达「叫什么」，表达不了「在哪个程序点住在哪」；
//   * 表在 IR 里随 CFG 一起被 pass 改写，改名/插指令/复制都必须显式处理它 ——
//     **编译器会点名**。这正是本文件不实现 `Clone`/`Default` 的原因（见下方编译期钉子）。
//
// 与后端的关系：本模块是 IR 侧的**生产者与载体**，消费家在 `mem/access.rs`、序言/尾声、
// asm 规划器、DWARF。类型本身（`Location`/`RegId`/`RegBank`）住在中立的 `crate::location`，
// 因为 Phase 4 要求 AST 侧也能携带家（-O0 与降级路径上没有 `ssa::Function`），
// 而 `ast` 不得依赖 `ssa`。本文件只留**版本粒度的表**（`LocMap`）与它的显式改写入口。

use crate::ast::Type;
use std::collections::{BTreeMap, HashMap};

// 类型只住在 `crate::location`，这里再导出：`pub use` 既把名字带进本模块作用域，
// 又让 `crate::ssa::Location` 仍是同一个类型（不是别名拷贝）。
pub use crate::location::{Location, RegBank, RegId, STACK_BACKEND_ASSIGNED};

/// SSA 名 → 家。键是**带版本号**的 SSA 名（`SsaVar::name()`，`x.2`），
/// 所以这张表的键生命周期受 SSA 改名与流水重编号影响 —— 正因如此它必须
/// 只有一个出处，且任何改写 SSA 名的 pass 都必须显式处理这张表。
///
/// 每个条目同时记下**基名与类型**，而不是到时候从名字里反解：反解需要重复一遍
/// `SsaVar::name()` 的编码规则，两处一旦漂移（分隔符、`version==0` 不拼后缀这条
/// 特例）折叠就会默默错基名 —— 而那正是 §7.15 以来反复出现的一族错。
/// 入口因此取 `SsaVar`（`bind`），而不是一个裸 `String`；类型一并存着是因为
/// `compute_splits` 给分裂出的真局部生成声明时要用它（另扫一遍 IR 就是第二份知道）。
///
/// **故意不实现 `Clone` 与 `Default`**：
///   * `Clone`：整份 `Function::clone()` 会顺带复制这张表。今天唯一的克隆点是软件流水
///     落地前的「克隆→验证→提交」（`pipelining::apply_plan`），那里**确实**该复制；
///     但把这件事交给 derive，就等于让「新增一个克隆点」静默通过。所以复制必须写成
///     `LocMap::fork()` —— 名字本身就是「我知道我在把位置表一起带走」的声明。
///     下面那个 trait 对是编译期钉子：一旦本类型实现 `Clone`，crate 就编不过。
///   * `Default`：默认空表会让「忘了建表」与「建了但一个值都不绑」长得一样，
///     而这两件事的产物差别很大。必须显式 `LocMap::empty()`。
///
/// `Debug` 是实现掉的：它不构成静默复制 hazard，而 `Function` 的 `dump` 与用例断言
/// 都要能把这张表打出来（Phase 4 的「每个值的家」视图也走这条路）。
#[derive(Debug)]
pub struct LocMap {
    map: HashMap<String, LocEntry>,
}

#[derive(Debug, Clone)]
struct LocEntry {
    base: String,
    loc: Location,
    ty: Type,
}

/// 一条基名的全部 SSA 版本条目（按 SSA 名升序），用于分裂判定。
type Versions = Vec<(String, Location, Type)>;

impl LocMap {
    pub fn empty() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// 显式复制（命名即声明语义）。`apply_plan` 这类「克隆整个函数再提交」的
    /// 路径用它，而不是依赖 derive 的静默行为。
    pub fn fork(&self) -> Self {
        Self {
            map: self.map.clone(),
        }
    }

    /// 绑定一个 SSA 值的家。基名与类型从 `SsaVar` 取，所以下面的折叠与分裂
    /// 都不需要反解名字、也不需要另扫一遍 IR 拿类型。
    pub fn bind(&mut self, v: &crate::ssa::ir::SsaVar, loc: Location) {
        self.map.insert(
            v.name(),
            LocEntry {
                base: v.base.clone(),
                loc,
                ty: v.ty.clone(),
            },
        );
    }

    pub fn get(&self, ssa_name: &str) -> Option<Location> {
        self.map.get(ssa_name).map(|e| e.loc)
    }

    pub fn base_of(&self, ssa_name: &str) -> Option<&str> {
        self.map.get(ssa_name).map(|e| e.base.as_str())
    }

    pub fn contains_key(&self, ssa_name: &str) -> bool {
        self.map.contains_key(ssa_name)
    }

    pub fn remove(&mut self, ssa_name: &str) -> bool {
        self.map.remove(ssa_name).is_some()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 读数按字典序（诊断与测试要的是确定序，不是 HashMap 的随机序）。
    pub fn sorted_entries(&self) -> Vec<(String, String, Location)> {
        let mut v: Vec<(String, String, Location)> = self
            .map
            .iter()
            .map(|(k, e)| (k.clone(), e.base.clone(), e.loc))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// 按基名分组（组内按 SSA 名升序 ⇒ 确定序）。
    fn grouped_by_base(&self) -> BTreeMap<String, Versions> {
        let mut by_base: BTreeMap<String, Versions> = BTreeMap::new();
        for (ssa, e) in self.map.iter() {
            by_base
                .entry(e.base.clone())
                .or_default()
                .push((ssa.clone(), e.loc, e.ty.clone()));
        }
        for vs in by_base.values_mut() {
            vs.sort_by(|a, b| a.0.cmp(&b.0));
        }
        by_base
    }

    /// 把版本粒度的表折叠成「基名 → 家」。
    ///
    /// 返回 `None` 的含义是「同基名的不同版本被分了不同的家」—— 今天的后端绑定表是
    /// **函数级** `名字 → 号`，读写路径每次查同一张表，中途没有换家的时机，所以这种
    /// 情形无法表示。它必须是**可计数、可点名**的，不能默默取其一是谁：
    /// 「一个号不得发给两个变量，以及它的反面「同基名塌缩后两个版本共享一个家」」
    /// 这两条判据就是用实测错值换来的（准入条件逐条记录在
    /// `docs/p12-physreg-spill-design.md` §1.1 第 7 条）。
    ///
    /// 输出序按基名字典序（确定序）。
    pub fn fold_by_base(&self) -> Vec<(String, Option<Location>)> {
        self.grouped_by_base()
            .into_iter()
            .map(|(base, vs)| {
                let agree = vs.iter().all(|(_, l, _)| *l == vs[0].1);
                (base, if agree { Some(vs[0].1) } else { None })
            })
            .collect()
    }

    /// 折叠到 **AST 局部名**（而非基名）：分裂后一个基名对应多个局部，每个有自己的家。
    ///
    /// 这是 `apply_bindings` 唯一的输入形式。不一致的家先交给 `compute_splits` 分裂；
    /// 真的不可表示（同基名、未分裂、家不同）才报 `None`，由交付侧点名而不是默默绑。
    pub fn fold_for_codegen(&self, splits: &Splits) -> Vec<(String, Option<Location>)> {
        let mut acc: BTreeMap<String, Option<Location>> = BTreeMap::new();
        for (ssa, e) in self.map.iter() {
            let local = splits
                .by_ssa
                .get(ssa)
                .cloned()
                .unwrap_or_else(|| e.base.clone());
            match acc.get(&local) {
                Some(Some(prev)) if *prev == e.loc => {},
                Some(_) => {
                    acc.insert(local, None);
                },
                None => {
                    acc.insert(local, Some(e.loc));
                },
            }
        }
        acc.into_iter().collect()
    }

    /// 已经**被占用的名字**的唯一口径：用户原始局部 + 形参 + IR 里出现过的全部定义基名
    /// （含 `_tbin_7` 这类合成临时值，也含软件流水造出来的轮转格）。
    ///
    /// 为什么这张名单要单独成为函数，而不是各调用方自己拼：分裂名与轮转格名都是
    /// 「凭空出来的名字换一个存储位置」，而名字一旦被两个对象共用，后端的
    /// 名字→家 / 名字→栈槽两张表就把它们当成同一个对象 —— 那正是 §7.20 那条判据
    /// （一个号不得发给两个变量）的反面形状。所以「什么算被占用」必须只有一份答案，
    /// 否则流水造的名字与分裂造的名字可以互相撞上而两边都自查通过。
    pub fn reserved_names(func: &crate::ssa::ir::Function) -> std::collections::HashSet<String> {
        let mut used: std::collections::HashSet<String> =
            func.original_locals.iter().cloned().collect();
        used.extend(func.params.iter().map(|p| p.base.clone()));
        for bb in func.blocks.values() {
            for phi in &bb.phis {
                used.insert(phi.dest.base.clone());
            }
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    used.insert(d.base.clone());
                }
            }
        }
        used
    }

    /// 分配一个分裂名：`{base}_s{k}`，`k` 从 `start` 起往上找第一个没被占用的；
    /// `used.insert` 同时就是登记，所以同一基名内与跨基名都不会重名。
    ///
    /// 选「最小可用 k」而不是加一个固定前缀：那能把「`x` 与 `x_s1` 已存在时的后续版本」
    /// 这样的链确定性地展开。这条口径**只有这一处实现**（`LocMap::compute_splits` 与
    /// 软件流水的轮转格共用它），谁再拼一次 `format!("{}_s{}", ..)` 就是第二份命名判据。
    pub fn reserve_split_name(
        used: &mut std::collections::HashSet<String>,
        base: &str,
        start: usize,
    ) -> String {
        let mut k = start;
        loop {
            let cand = format!("{}_s{}", base, k);
            if used.insert(cand.clone()) {
                return cand;
            }
            k += 1;
        }
    }

    /// 由本表算出版本分裂表：**只允许分裂用户局部变量**。
    /// 自动分配本来就把 `not-a-user-local` 当拒因，而合成临时值的基名还被
    /// `is_synthetic_temp_base` 按前缀判过（改名会让那条判据隔两层不一致）。
    /// 临时值要拿不同的家，得先连那条一起设计，不在这里默默改名。
    ///
    /// 做成方法而不是另扫一遍 IR：它要用的是本表的条目与 `original_locals`，
    /// 两者都已在手上，再写一份“哪些版本属于同一个基名”的判据就是第二份真源。
    pub fn compute_splits(&self, func: &crate::ssa::ir::Function) -> Splits {
        let mut out = Splits::default();
        // 占名检查。分裂名以前是「拼出来就用」（`{base}_s{i}`），而用户完全可以已经有一个
        // 叫 `x_s1` 的局部：那时分裂体与它**同名**，而后端的名字→家 / 名字→栈槽两张表
        // 都是按名字索引的，同名就是两个对象共用一个家 —— 正是 §7.20 那条判据（一个号
        // 不得发给两个变量）的反面形状，而它今天只在「用户起了这个名」时发生，所以
        // 靠现有语料根本碰不到。名单口径见 `Self::reserved_names`（原始局部 + 形参 + IR 里
        // 出现过的全部定义基名 + 本表基名），分配走 `Self::reserve_split_name`（与本文件
        // 唯一实现）：软件流水的轮转格也走这两个入口，两边共用一份答案才不会互相撞上。
        let mut used: std::collections::HashSet<String> = Self::reserved_names(func);
        used.extend(self.map.values().map(|e| e.base.clone()));
        for (base, vs) in self.grouped_by_base() {
            if vs.len() < 2 || vs.iter().all(|(_, l, _)| *l == vs[0].1) {
                continue; // 只有一个版本，或版本们同一个家 ⇒ 不需要分裂
            }
            if !func.original_locals.contains(&base) {
                continue; // 不可分裂：见本函数的注释
            }
            // 升序后首个版本沿用原名（它在 `original_locals` 里已有声明），
            // 其余版本拿到带后缀的新局部名 + 自己的类型。
            for (i, (ssa, _, ty)) in vs.iter().enumerate() {
                let ast = if i == 0 {
                    base.clone()
                } else {
                    Self::reserve_split_name(&mut used, base.as_str(), i)
                };
                out.by_ssa.insert(ssa.clone(), ast.clone());
                if i > 0 {
                    out.locals.push((ast, base.clone(), ty.clone()));
                }
            }
        }
        out
    }

    /// SSA 改名时同步键。`rename(old, new)` 用于 `rename_var` 那类**全函数**改名；
    /// 返回是否真的动了，调用方据此判断有没有丢过条目。
    pub fn rename(&mut self, old: &str, new: &str) -> bool {
        if old == new {
            return false;
        }
        match self.map.remove(old) {
            Some(e) => {
                self.map.insert(new.to_string(), e);
                true
            },
            None => false,
        }
    }

    /// 丢弃不再存在的定义（删指令/删块之后调用）。返回被丢掉的条目数，供自检：
    /// 「表里有键却找不到对应的 def」是**静默错值**的形状（家挂在死名字上，
    /// 新名字默默落默认值），所以这件事必须可计数，不能让 `dangling` 只存在于 IR 侧。
    pub fn retain_existing(&mut self, existing: impl Fn(&str) -> bool) -> usize {
        let before = self.map.len();
        self.map.retain(|k, _| existing(k));
        before - self.map.len()
    }

    /// 自检：返回表里那些**在本函数已找不到定义**的键（确定序）。
    /// 与 `cfg.rs::dangling_refs` / `duplicate_ssa_defs` 同族，判据也同族。
    pub fn dangling_keys(&self, defined: impl Fn(&str) -> bool) -> Vec<String> {
        let mut v: Vec<String> = self.map.keys().filter(|k| !defined(k)).cloned().collect();
        v.sort();
        v
    }
}

/// **D3 / Phase 3.4：按 Location 生成真局部。**
///
/// 今天 P12-A 只能按**基名**绑，因为重建把 `x.1/x.2/x.3` 塌缩回同一个可变量 `x`；
/// 同一个基名要拿两个不同的家，在旧形状下不可表示（所以 `fold_by_base` 报 `None`）。
/// 那就是本方案量化到的 23.57% 池满拒绑的结构性原因之一：分配器再聪明，
/// 基名塌缩下也拿不到版本级的收益（设计 §11.1.1 表 B 的 357 vs 13 就是这个差距）。
///
/// 本类型就是把「哪些版本必须成为各自的 AST 局部」这件事算一次、存到
/// `Function::local_splits`，让重建（声明与读写）与后端交付（`apply_bindings`）读同一个答案。
///
/// **今天它总是空**：分配器给同基名的每个版本同一个家，所以产物逐字节不变；
/// 它的价值在于「一旦不一致，真局部会自动出现」而不是默默拒绑。
#[derive(Debug, Clone, Default)]
pub struct Splits {
    /// SSA 名 → 分裂后的 AST 局部名（只有被分裂的版本才在里面）
    by_ssa: HashMap<String, String>,
    /// 需要新增声明的局部：(AST 局部名, 基名, 类型)
    locals: Vec<(String, String, Type)>,
}

impl Splits {
    /// SSA 版本在 AST 里该用的局部名（没分裂就是基名）。
    pub fn ast_name(&self, v: &crate::ssa::ir::SsaVar) -> String {
        self.by_ssa
            .get(&v.name())
            .cloned()
            .unwrap_or_else(|| v.base.clone())
    }

    /// 分裂名字 → 它的类型（重建生成声明用）。
    pub fn new_locals(&self) -> &[(String, String, Type)] {
        &self.locals
    }

    /// AST 局部名 → 它的类型（仅分裂出的新名字有）。重建那边选类型的链条上
    /// `local_types`/`all_def_types` 都只认基名，分裂名必须在这里拿到自己的类型，
    /// 否则它会退到 `Type::Int` 并按 4 字节分配栈槽 —— 又是一次静默宽度错。
    pub fn type_of_local(&self, ast_name: &str) -> Option<Type> {
        self.locals
            .iter()
            .find(|(n, _, _)| n == ast_name)
            .map(|(_, _, ty)| ty.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.by_ssa.is_empty()
    }
}

/// 版本分裂表的类型判据住在 `LocMap::compute_splits`（它要用本表的条目与
/// `original_locals`，两者都在手上）。这里不再放一个同名自由函数：两份入口就是
/// 两份「哪些版本属于同一个基名」的知道，一漂就错得静默。

// ── 编译期钉子：LocMap 必须**不**实现 Clone ──────────────────────────────────
//
// 这两个 trait 对的含义：`impl<T: Clone> LocIsClone for T {}` 是通用实现，
// `impl LocIsClone for LocMap {}` 是具体实现。**只要 `LocMap` 实现了 `Clone`**，
// 两者就重叠，rustc 直接报 E0119 —— 也就是「有人给这张表加了 derive(Clone)」
// 这件事的代价从「静默复制位置表」变成「crate 编不过」。
//
// 这不是装饰：`docs/p12-physreg-spill-design.md` §7 Phase 3.3 要的正是
// 「让编译器点名，不留兜底」。`Function` 带着 `#[derive(Clone)]`，新字段若可复制，
// 4 个克隆点一个都不会报错（实测见 §11.5 第 2 条）。
mod clone_guard {
    use super::LocMap;

    #[allow(dead_code)]
    pub trait LocIsClone {}
    impl<T: Clone> LocIsClone for T {}
    impl LocIsClone for LocMap {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Type;
    use crate::diagnostics::Span;

    fn v(base: &str, version: u32) -> crate::ssa::ir::SsaVar {
        crate::ssa::ir::SsaVar {
            base: base.to_string(),
            version,
            ty: Type::Long,
            span: Span::point("t.c", 1, 1),
        }
    }

    fn gp(n: u8) -> Location {
        Location::Reg(RegId {
            bank: RegBank::Gp,
            n,
        })
    }

    /// `fork` 必须是**独立**副本：改副本不能影响原件。
    /// （`apply_plan` 的「克隆→验证→提交」靠的就是这个性质。）
    #[test]
    fn fork_produces_an_independent_copy() {
        let mut m = LocMap::empty();
        m.bind(&v("x", 1), gp(3));
        let mut f = m.fork();
        assert_eq!(f.len(), 1);
        f.bind(&v("y", 2), Location::backend_assigned_stack());
        assert_eq!(m.len(), 1, "改副本不得动原件");
        assert_eq!(f.len(), 2);
        assert_eq!(f.get("x.1"), Some(gp(3)));
    }

    /// 键必须与 `SsaVar::name()` 同一形式（`version==0` 不拼后缀）。
    /// 这条看着 trivial，但它是「表能不能查到」的全部：两边形式不一致时查不到家
    /// 不会报错，只会默默退回默认位。所以用名字而不是用 `v.name()` 直接验证。
    #[test]
    fn keys_are_the_ssa_name_form() {
        let mut m = LocMap::empty();
        m.bind(&v("x", 0), gp(3));
        m.bind(&v("x", 2), gp(3));
        assert!(m.contains_key("x"), "version==0 的名字就是基名，不得拼成 `x.0`");
        assert!(m.contains_key("x.2"));
        assert!(!m.contains_key("x.0"));
        // 基名随条目记录，不靠反解
        let e = m.sorted_entries();
        assert_eq!(e[0].1, "x");
        assert_eq!(e[1].1, "x");
    }

    /// 折叠规则：同基名全同才给家；不一致 ⇒ `None`（今天不可表示），而且不能丢条目。
    #[test]
    fn fold_requires_versions_of_a_base_to_agree() {
        // (a) 同基名两个版本共享同一个家 ⇒ 可表示
        let mut m = LocMap::empty();
        m.bind(&v("i", 0), gp(3));
        m.bind(&v("i", 1), gp(3));
        m.bind(&v("j", 0), gp(12));
        assert_eq!(
            m.fold_by_base(),
            vec![
                ("i".to_string(), Some(gp(3))),
                ("j".to_string(), Some(gp(12)))
            ]
        );

        // (b) 同基名不同号 ⇒ 不可表示，必须点名而不是默默取一个
        let mut bad = LocMap::empty();
        bad.bind(&v("i", 0), gp(3));
        bad.bind(&v("i", 1), gp(12));
        assert_eq!(bad.fold_by_base(), vec![("i".to_string(), None)]);
        assert_eq!(bad.len(), 2, "折叠不得改变表本身");

        // (c) 占位家与寄存器家混在一个基名上也算不可表示
        let mut mixed = LocMap::empty();
        mixed.bind(&v("k", 0), gp(3));
        mixed.bind(&v("k", 1), Location::backend_assigned_stack());
        assert_eq!(mixed.fold_by_base(), vec![("k".to_string(), None)]);
    }

    /// 编号域按通道解释：`n` 相同的两个号不是同一个家。
    #[test]
    fn reg_numbers_are_per_bank() {
        let gp0 = Location::Reg(RegId {
            bank: RegBank::Gp,
            n: 0,
        });
        let simd0 = Location::Reg(RegId {
            bank: RegBank::Simd,
            n: 0,
        });
        assert_ne!(gp0, simd0, "同一个数在两个通道里是两个不同的寄存器");
        let mut m = LocMap::empty();
        m.bind(&v("a", 0), gp0);
        m.bind(&v("b", 0), simd0);
        assert_eq!(m.get("a"), Some(gp0));
        assert_eq!(m.get("b").and_then(|l| l.reg()).map(|r| r.bank), Some(RegBank::Simd));
        // 两个通道各一个号 ⇒ 折叠后两个基名都仍可表示（不因 `n` 相同而误判不一致）
        assert_eq!(m.fold_by_base().iter().filter(|(_, l)| l.is_none()).count(), 0);
    }

    /// 「后端定槽」的占位与真实偏移 0 不得混淆（类型本身的用例在 `crate::location`，
    /// 这里只验它对**表与折叠**的影响：占位也是一个家，与寄存器家不一致 ⇒ 不可表示）。
    #[test]
    fn placeholder_home_is_a_home_too_for_folding() {
        let mut m = LocMap::empty();
        m.bind(&v("k", 0), Location::backend_assigned_stack());
        m.bind(&v("k", 1), Location::backend_assigned_stack());
        // 全同的占位仍是一个一致的家（折叠不报不可表示），但它是「未定」而不是偏移
        let folded = m.fold_by_base();
        assert_eq!(folded.len(), 1);
        assert!(matches!(folded[0].1, Some(Location::Stack(_))));
        assert!(!folded[0].1.unwrap().is_resolved());
    }

    #[test]
    fn rename_and_drop_are_explicit_and_countable() {
        let mut m = LocMap::empty();
        m.bind(&v("x", 1), Location::Stack(8));
        assert_eq!(m.get("x.1"), Some(Location::Stack(8)));
        assert!(m.rename("x.1", "x.2"), "改名必须报告它动了条目");
        assert!(!m.rename("x.1", "gone"), "没有该键时不得谎报");
        assert!(m.contains_key("x.2") && !m.contains_key("x.1"));
        // 改名不能弄丢基名记录（否则折叠会错基名）
        assert_eq!(m.sorted_entries()[0].1, "x");

        m.bind(&v("dead", 1), Location::Stack(16));
        let dropped = m.retain_existing(|k| k.starts_with("x"));
        assert_eq!(dropped, 1);
        assert!(m.dangling_keys(|k| k.starts_with("x")).is_empty());
        assert_eq!(m.dangling_keys(|_| false), vec!["x.2".to_string()]);
    }

    /// 端到端：重建必须**真的**按分裂表生成真局部，且读写都跟着改名。
    ///
    /// 上面的用例只验了表本身；而“多一个声明、漏一个改名”会产生一个不存在的
    /// 名字（后端按隐式 int 处理）或两个版本写到同一个格子 —— 两者都是静默错值，
    /// 所以这里直接跑 `build_from_func → 造表 → rebuild_block`，验三件事：
    /// 新局部有声明、赋值落在它身上、读取也用它。
    #[test]
    fn rebuild_emits_real_locals_per_location() {
        let src = "long f(void){long x=1;x=x+2;return x;}\n";
        let (prog, _d) = crate::parser::parse(src.as_bytes(), "t.c");
        let (m, _w) = crate::ir::lower(prog).unwrap();
        let fd = m
            .source()
            .decls
            .iter()
            .find_map(|d| match d {
                crate::ast::Decl::Func(f) if f.name.name == "f" => Some(f.clone()),
                _ => None,
            })
            .expect("f 应在源里");
        let mut func = crate::ssa::build_from_func(&fd).expect("f 应能 SSA 化");
        // 收集 x 的版本名（不靠猜：从 IR 里拿）
        let mut versions: Vec<String> = Vec::new();
        for bb in func.blocks.values() {
            for inst in &bb.insts {
                if let Some(d) = &inst.dest {
                    if d.base == "x" {
                        versions.push(d.name());
                    }
                }
            }
        }
        versions.sort();
        versions.dedup();
        assert!(versions.len() >= 2, "这条用例需要 x 至少两个 SSA 版本，实得 {versions:?}");
        // 故意给两个版本不同的家 → 必须分裂
        let mut m2 = LocMap::empty();
        for (i, name) in versions.iter().enumerate() {
            let var = crate::ssa::ir::SsaVar {
                base: "x".to_string(),
                version: i as u32 + 1,
                ty: Type::Long,
                span: Span::point("t.c", 1, 1),
            };
            assert_eq!(&var.name(), name, "版本编号假设变了，用例得跟着改");
            m2.bind(&var, gp(if i == 0 { 3 } else { 12 }));
        }
        func.value_loc = m2;
        let s = func.value_loc.compute_splits(&func);
        assert!(!s.is_empty());
        func.local_splits = s.clone();

        let body = crate::ssa::rebuild_block(&func);
        let text = format!("{:?}", body);
        // 1) 分裂出的局部被声明了（不在 original_locals 里，必须重建新增）
        let mut declared = false;
        let mut assigned = false;
        fn scan(b: &crate::ast::Block, declared: &mut bool, assigned: &mut bool) {
            for item in &b.items {
                match item {
                    crate::ast::BlockItem::Decl(crate::ast::Decl::Var(v)) => {
                        if v.name.name.ends_with("_s1") {
                            *declared = true;
                            assert_eq!(v.ty, Type::Long, "分裂局部的类型不能退成 int");
                        }
                    },
                    crate::ast::BlockItem::Stmt(s) => scan_stmt(s, declared, assigned),
                    _ => {},
                }
            }
        }
        fn scan_stmt(s: &crate::ast::Stmt, declared: &mut bool, assigned: &mut bool) {
            match s {
                crate::ast::Stmt::Expr { e, .. } => scan_expr(e, declared, assigned),
                crate::ast::Stmt::Block { b, .. } => scan(b, declared, assigned),
                crate::ast::Stmt::While { body, .. }
                | crate::ast::Stmt::Do { body, .. }
                | crate::ast::Stmt::For { body, .. } => scan_stmt(body, declared, assigned),
                crate::ast::Stmt::Switch { cases, .. } => {
                    for c in cases {
                        scan(&c.body, declared, assigned);
                    }
                },
                crate::ast::Stmt::If { then_, else_, .. } => {
                    scan_stmt(then_, declared, assigned);
                    if let Some(e) = else_ {
                        scan_stmt(e, declared, assigned);
                    }
                },
                _ => {},
            }
        }
        fn scan_expr(e: &crate::ast::Expr, declared: &mut bool, assigned: &mut bool) {
            match e {
                crate::ast::Expr::Assign { l, r, .. } => {
                    if let crate::ast::Expr::Ident { name, .. } = &**l {
                        if name.name.ends_with("_s1") {
                            *assigned = true;
                        }
                    }
                    scan_expr(r, declared, assigned);
                },
                crate::ast::Expr::Ident { name, .. } => {
                    if name.name.ends_with("_s1") {
                        *assigned |= true;
                    }
                },
                crate::ast::Expr::Binop { l, r, .. } | crate::ast::Expr::Comma { l, r, .. } => {
                    scan_expr(l, declared, assigned);
                    scan_expr(r, declared, assigned);
                },
                crate::ast::Expr::Unary { e, .. }
                | crate::ast::Expr::Cast { e, .. }
                | crate::ast::Expr::Deref { e, .. } => scan_expr(e, declared, assigned),
                _ => {},
            }
        }
        scan(&body, &mut declared, &mut assigned);
        assert!(declared, "分裂局部必须有声明，实得体：{text}");
        assert!(assigned, "读写必须落在分裂局部上，实得体：{text}");
    }

    /// 读数必须是确定序（本仓实测过 HashMap 迭代序泄进诊断文本/产物的坑）。
    #[test]
    fn readouts_are_deterministic() {
        let mut m = LocMap::empty();
        for (b, ver) in [("c", 1), ("a", 2), ("b", 1), ("a", 1)] {
            m.bind(&v(b, ver), gp(3));
        }
        let keys: Vec<String> = m.sorted_entries().into_iter().map(|(k, ..)| k).collect();
        assert_eq!(keys, vec!["a.1", "a.2", "b.1", "c.1"]);
        let folded: Vec<String> = m.fold_by_base().into_iter().map(|(b, _)| b).collect();
        assert_eq!(folded, vec!["a", "b", "c"]);
    }

    /// D3 / Phase 3.4：**同基名的版本拿到不同家时，必须分裂成各自的 AST 局部**。
    ///
    /// 今天这张表总是空（分配器给同基名每个版本同一个家），所以上面几条验的都是
    /// 「不分裂」。本用例故意绕过分配器直接给两个不同的家，验机制本身在：分裂名、
    /// 新局部带自己的类型、交付折叠后两个局部各自有家（而不是报「不可表示」）。
    /// 没这些机制，分配器再聪明也只能按基名绑 —— 那正是 23.57% 池满拒绑的结构原因。
    #[test]
    fn conflicting_homes_split_into_real_locals() {
        let mut func =
            crate::ssa::ir::Function::new("f".to_string(), Type::Long, Span::point("t.c", 1, 1));
        func.original_locals.insert("x".to_string());
        let mut m = LocMap::empty();
        m.bind(&v("x", 1), gp(3));
        m.bind(&v("x", 2), gp(12));
        func.value_loc = m;

        let s = func.value_loc.compute_splits(&func);
        assert!(!s.is_empty(), "不同家必须分裂");
        assert_eq!(s.ast_name(&v("x", 1)), "x", "首个版本沿用原名（已有声明）");
        assert_eq!(s.ast_name(&v("x", 2)), "x_s1");
        assert_eq!(s.new_locals().len(), 1);
        assert_eq!(
            s.type_of_local("x_s1"),
            Some(Type::Long),
            "新局部必须带自己的类型，否则声明会退成 int 并按 4 字节分栈槽"
        );
        let folded = func.value_loc.fold_for_codegen(&s);
        assert_eq!(
            folded,
            vec![
                ("x".to_string(), Some(gp(3))),
                ("x_s1".to_string(), Some(gp(12))),
            ],
            "分裂必须消除基名层的冲突，而不是默默丢一个"
        );
    }

    /// 分裂名的**占名检查**（§11.13 给 5.1.e 点名的两条表达缺口之一）。
    ///
    /// 用户自己可以有一个叫 `x_s1` 的局部。以前分裂体无条件拿这个名字，于是
    /// 「两个不同的对象共用一个名字」—— 后端的名字→家、名字→栈槽两张表都按名字索引，
    /// 同名就是共用一个家（§7.20 那条判据的反面形状）。本用例同时钉三件事：
    ///   * 分裂名不得等于任何原始局部；
    ///   * 分裂名之间也不得互相同名（多个基名同时分裂时）；
    ///   * 首个版本仍用原名（那是它已有的声明），且新名字只往后扰。
    #[test]
    fn split_names_cannot_collide_with_user_locals() {
        let mut func =
            crate::ssa::ir::Function::new("f".to_string(), Type::Long, Span::point("t.c", 1, 1));
        // 用户同时有 `x`/`y`（会被分裂）与 `x_s1`/`x_s2`/`y_s1`（不该被碰）
        for name in ["x", "y", "x_s1", "x_s2", "y_s1"] {
            func.original_locals.insert(name.to_string());
        }
        let mut m = LocMap::empty();
        // 两个基名都拿到不同的家（同基名内不同一个家才会分裂）
        m.bind(&v("x", 1), gp(3));
        m.bind(&v("x", 2), gp(12));
        m.bind(&v("x", 3), gp(14));
        m.bind(&v("y", 1), gp(3));
        m.bind(&v("y", 2), gp(15));
        func.value_loc = m;

        let s = func.value_loc.compute_splits(&func);
        let made: Vec<String> = s.new_locals().into_iter().map(|t| t.0.clone()).collect();
        // 1) 不得踩到用户的名字
        for name in &made {
            assert!(
                !func.original_locals.contains(name.as_str()),
                "分裂名 '{name}' 撞上了一个已有局部"
            );
        }
        // 2) 不得自相同名
        let uniq: std::collections::HashSet<&String> = made.iter().collect();
        assert_eq!(uniq.len(), made.len(), "分裂名之间不得重名：{made:?}");
        // 3) 具体形状：x 的第 2/3 版本因 `x_s1`/`x_s2` 已被占而拿到 `x_s3`/`x_s4`；
        //    y 的第 2 版本拿 `y_s2`（`y_s1` 是用户的名字，从 i=1 起被占 ⇒ 扰到 2）
        assert_eq!(s.ast_name(&v("x", 1)), "x", "首个版本必须沿用原名");
        assert_eq!(s.ast_name(&v("x", 2)), "x_s3", "实得：{made:?}");
        assert_eq!(s.ast_name(&v("x", 3)), "x_s4", "实得：{made:?}");
        assert_eq!(s.ast_name(&v("y", 2)), "y_s2", "实得：{made:?}");
        // 反向对照：没被占时就是 `x_s1`（上面的判据不能是「永远往后推一格」）
        let mut plain =
            crate::ssa::ir::Function::new("g".to_string(), Type::Long, Span::point("t.c", 1, 1));
        plain.original_locals.insert("x".to_string());
        let mut pm = LocMap::empty();
        pm.bind(&v("x", 1), gp(3));
        pm.bind(&v("x", 2), gp(12));
        plain.value_loc = pm;
        assert_eq!(
            plain.value_loc.compute_splits(&plain).ast_name(&v("x", 2)),
            "x_s1",
            "没名字可占时不得多推一格后缀（那会让已有用例的名字与产物都漂）"
        );
    }

    /// 同一个家（今天的全部情况）⇒ 不分裂，且交付折叠与基名折叠一致。
    /// 这条是「产物逐字节不变」在机制层的理由：空分裂表不改变任何名字。
    #[test]
    fn agreeing_homes_do_not_split() {
        let mut func =
            crate::ssa::ir::Function::new("f".to_string(), Type::Long, Span::point("t.c", 1, 1));
        func.original_locals.insert("x".to_string());
        let mut m = LocMap::empty();
        m.bind(&v("x", 1), gp(3));
        m.bind(&v("x", 2), gp(3));
        func.value_loc = m;

        let s = func.value_loc.compute_splits(&func);
        assert!(s.is_empty(), "同一个家不得分裂（否则白多一个局部）");
        assert_eq!(s.ast_name(&v("x", 2)), "x");
        assert_eq!(
            func.value_loc.fold_for_codegen(&s),
            func.value_loc.fold_by_base(),
            "空分裂表下交付折叠必须等同基名折叠"
        );
    }

    /// 非用户局部（合成临时值）**不得**被改名：它的基名还被 `is_synthetic_temp_base`
    /// 按前缀判过，改名会让那条判据隔两层不一（同一事实两份判据 = 静默错值）。
    #[test]
    fn synthetic_temps_are_never_renamed() {
        let mut func =
            crate::ssa::ir::Function::new("f".to_string(), Type::Long, Span::point("t.c", 1, 1));
        let mut m = LocMap::empty();
        m.bind(&v("_tbin_7", 1), gp(3));
        m.bind(&v("_tbin_7", 2), gp(12));
        func.value_loc = m;
        let s = func.value_loc.compute_splits(&func);
        assert!(
            s.is_empty(),
            "临时值不得分裂：它们的家要能分得先连 is_synthetic_temp_base 一起设计"
        );
    }
}
