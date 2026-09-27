//! M26: C++ 模板完整单态化 (monomorphization)
//!
//! 职责:
//! 1. 收集模板声明 (模板类 / 模板函数 / 偏特化)
//! 2. 按类型实参实例化: 模板体内的类型形参替换为实参 (AST 级替换)
//! 3. 实例化缓存: 相同实参组合只实例化一次
//! 4. 模板函数隐式实参推导 (从调用实参类型推导 T)
//! 5. requires 约束求值 (概念过滤: 约束不满足则实例化失败 → SFINAE)
//!
//! 设计原则: 实例化 = 纯 AST 变换, 不依赖 lower 上下文;
//! 隐式推导需要调用点类型信息, 由 lower 计算好实参类型后调用.

use crate::cpp::feature_degrade::SfinaeCtx;
use crate::cpp::mangle::MangleType;
use crate::cpp::parser::{
    ClassDecl, CppBody, CppDecl, CppExpr, CppStmt, DeclSpec, InitDeclarator, SizeofArg,
    TemplateArgs,
};

/// 按字节切分顶层模板实参列表 (逗号在 `<>` / `()` 内不算分隔符)
fn split_template_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        match b {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' | b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(text[start..i].to_string());
                start = i + 1;
            },
            _ => {},
        }
    }
    let last = text[start..].to_string();
    if !last.trim().is_empty() || !out.is_empty() {
        out.push(last);
    }
    out
}

/// 从 `open` 指向的 `<` 起找到配对的 `>` 下标 (字符串结束找不到返回 None)
fn find_angle_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            },
            _ => {},
        }
    }
    None
}

/// G13: Result of template matching with priority resolution
#[derive(Debug, Clone)]
pub enum BestMatchResult<'a> {
    FullSpec(&'a FullSpecEntry),
    /// 偏特化命中: (实例化后的声明, 形参名表, 实参表).
    /// `CppDecl` 远大于其它 variant 的负载, 故整体 `Box` 化以免
    /// `BestMatchResult` 被撑大 (变体尺寸差 > clippy 阈值).
    PartialSpec(Box<(CppDecl, Vec<String>, Vec<MangleType>)>),
    Primary(&'a TemplateDecl),
    NotFound,
}

/// 模板声明 (类模板或函数模板的统一包装)
#[derive(Debug, Clone)]
pub struct TemplateDecl {
    /// 模板名 (不含命名空间)
    pub name: String,
    /// 类型形参名列表 (T / U / ...)
    pub param_names: Vec<String>,
    /// 各形参默认实参
    pub defaults: Vec<Option<MangleType>>,
    /// requires 约束文本 (M26: 实例化时求值过滤)
    pub requires: Option<String>,
    /// 模板体 (Class 或 FreeFn)
    pub decl: CppDecl,
    /// G12: 记录哪些形参是 pack
    pub is_variadic_param: Vec<bool>, // matches param_names
    /// G11: 原始模板参数定义（用于识别模板模板参数）
    pub template_params: Vec<crate::cpp::parser::TemplateParam>,
}

impl TemplateDecl {
    /// M51: 模板去重指纹: 名称 + 形参名表 + requires + 函数签名.
    /// 只在同名模板冲突时计算, 用来区分"重复 include 的同一个模板"(指纹相同,
    /// 后者覆盖) 和"同名重载模板" (指纹不同, 两个候选都保留).
    pub fn fingerprint(&self) -> String {
        let mut s = String::new();
        s.push_str(&self.name);
        s.push('|');
        s.push_str(&self.param_names.join(","));
        s.push('|');
        s.push_str(self.requires.as_deref().unwrap_or(""));
        s.push('|');
        match &self.decl {
            CppDecl::FreeFn { ret, params, .. } => {
                s.push_str("fn(");
                for (n, t) in params {
                    s.push_str(n);
                    s.push(':');
                    s.push_str(&type_key(t));
                    s.push(',');
                }
                s.push_str("):");
                s.push_str(&type_key(ret));
            }
            CppDecl::Class(_) => s.push_str("class"),
            _ => s.push_str("other"),
        }
        s
    }
}

/// 偏特化条目：`template <typename T> class Foo<T*> { ... }`
#[derive(Debug, Clone)]
pub struct PartialSpecEntry {
    pub name: String,
    pub param_names: Vec<String>,
    /// 特化模式，逗号分隔的多形参文本 (M23 增强)
    pub pattern_text: String,
    pub decl: CppDecl,
}

/// G13: 全特化条目：`template<> class Foo<void> { ... }`
#[derive(Debug, Clone)]
pub struct FullSpecEntry {
    pub name: String,
    /// 指定的类型实参 [void] 或 [int, double] 等
    pub specific_args: Vec<MangleType>,
    pub body: crate::cpp::parser::CppBody,
}

/// 模板注册表 + 实例化缓存
#[derive(Debug, Clone, Default)]
pub struct TemplateRegistry {
    pub templates: Vec<TemplateDecl>,
    pub partials: Vec<PartialSpecEntry>,
    /// G13: 全特化注册表
    pub full_specs: Vec<FullSpecEntry>,
    /// 实例化缓存：key = `name<arg0,arg1,...>` 编码 → 实例化结果
    pub inst_cache: std::collections::HashMap<String, CppDecl>,
    /// 实例化顺序记录 (key 列表，供 lower 按依赖序输出)
    pub inst_order: Vec<String>,
    /// M26: 概念定义注册表 (concept name -> body_text)
    pub concepts: std::collections::HashMap<String, String>,
    /// M51: 概念的形参名表 (concept name -> [T, U, ...]), 用于把概念体中的
    /// 形参正确绑定到使用点实参 (概念形参名与外层模板形参名不必一致)
    pub concept_params: std::collections::HashMap<String, Vec<String>>,
    /// M51: 模板/概念定义在顶层声明序列中的锚点 (前缀存活声明数),
    /// 实例化结果按此锚点插回定义点之后, 保持 C++ 的"定义先于使用"顺序
    pub anchors: std::collections::HashMap<String, usize>,
    /// G12/M52: 参数包摊平失败的首个原因.
    ///
    /// 必须向调用方报硬错: 不能把"摊平失败"退化成 `None` —— 那会被上游误判为
    /// "候选集为空 (SFINAE)", 把实现缺陷伪装成合法的重载决议失败.
    pub pack_expand_error: Option<String>,
}

/// 把 MangleType 编码为实例化缓存 key (与 C 标识符命名一致)
pub fn type_key(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".into(),
        MangleType::Bool => "bool".into(),
        MangleType::Char => "char".into(),
        MangleType::Schar => "schar".into(),
        MangleType::Uchar => "uchar".into(),
        MangleType::Short => "short".into(),
        MangleType::Ushort => "ushort".into(),
        MangleType::Int => "int".into(),
        MangleType::Uint => "uint".into(),
        MangleType::Long => "long".into(),
        MangleType::Ulong => "ulong".into(),
        MangleType::LongLong => "llong".into(),
        MangleType::UlongLong => "ullong".into(),
        MangleType::Float => "float".into(),
        MangleType::Double => "double".into(),
        MangleType::LongDouble => "ldouble".into(),
        MangleType::Pointer(t) => format!("p_{}", type_key(t)),
        MangleType::Reference(t) => format!("r_{}", type_key(t)),
        MangleType::Class(n) => n.replace("::", "_"),
        MangleType::Template { name, args } => {
            let flat_name = name.replace("::", "_");
            let mut s = format!("t_{}", flat_name);
            for a in args {
                s.push('_');
                s.push_str(&type_key(a));
            }
            s
        },
        MangleType::TplMember { name, args, member } => {
            let flat_name = name.replace("::", "_");
            let mut s = format!("tm_{}", flat_name);
            for a in args {
                s.push('_');
                s.push_str(&type_key(a));
            }
            s.push('_');
            s.push_str(member);
            s
        },
        // G11: 模板模板参数符号
        MangleType::TemplateParam(sym) => {
            format!("tt_{}", sym.0.replace("::", "_"))
        },
        // G12: 参数包展开 - 将包内类型都转换为 key 并连接
        MangleType::VariadicPack(packed) => {
            let mut s = String::from("pack_");
            packed.iter().for_each(|t| {
                s.push('_');
                s.push_str(&type_key(t));
            });
            s
        },
        // M30.1: 非类型实参 key `n<值>` (n0/n1/n42); 偏特化 pattern
        // 字面量 (true/false/整数) 由 pick_partial 解析后与之比较
        MangleType::Array(t, n) => format!("arr{}_{}", n, type_key(t)),
        // 函数类型 key: 保留完整签名，使 `void(*)(int)` 与
        // `void(*)(long)` 不会撞同一个实例化缓存 key。
        MangleType::Function { ret, params } => {
            let mut s = format!("fn_{}", type_key(ret));
            for p in params {
                s.push('_');
                s.push_str(&type_key(p));
            }
            s
        },
        MangleType::NonType(v) => format!("n{}", v),
        MangleType::Ellipsis => "ellipsis".into(),
        MangleType::Auto => "auto".into(),
        MangleType::DecltypeAuto => "decltype_auto".into(),
    }
}

/// 实例化后的类名：`Box<int>` → `__dcc_template_Box_int`
/// 必须与 lower.rs `mangle_to_c` 的 Template 分支一致.
pub fn instantiated_class_name(name: &str, args: &[MangleType]) -> String {
    // G12: Expand any variadic packs first
    let expanded_args = merge_expanded_args(args);

    // Flatten namespace qualifiers: std::vector -> std_vector
    let flat_name = name.replace("::", "_");
    let mut s = format!("__dcc_template_{}", flat_name);
    for a in &expanded_args {
        s.push('_');
        s.push_str(&type_key(a));
    }
    s
}

// ===== G12: Variadic Template Support =====

/// 将单个 MangleType 或 VariadicPack 展开为 Vec<MangleType>
pub fn expand_pack(ty: &MangleType) -> Vec<MangleType> {
    match ty {
        MangleType::VariadicPack(packed) => packed.clone(),
        other => vec![other.clone()],
    }
}

/// 合并多个类型的包展开 (处理 `args...` 在实参列表中的情况)
pub fn merge_expanded_args(args: &[MangleType]) -> Vec<MangleType> {
    args.iter().flat_map(expand_pack).collect()
}

impl TemplateRegistry {
    /// 从 TemplateArgs 构造 TemplateDecl (解析阶段收集用)
    pub fn decl_from(
        targs: &TemplateArgs,
        requires: &Option<String>,
        decl: CppDecl,
    ) -> Option<TemplateDecl> {
        let name = match &decl {
            CppDecl::Class(c) => c.name.clone(),
            CppDecl::FreeFn { name, .. } => name.clone(),
            _ => return None,
        };
        // G12: Determine which params are packs
        let is_variadic_param: Vec<bool> = targs.params.iter().map(|p| p.is_pack).collect();

        Some(TemplateDecl {
            name,
            param_names: targs.params.iter().map(|p| p.name.clone()).collect(),
            defaults: targs.params.iter().map(|p| p.default.clone()).collect(),
            requires: requires.clone(),
            decl,
            is_variadic_param,
            template_params: targs.params.clone(),
        })
    }

    /// G12: 检查 template 是否包含 variadic pack params
    pub fn has_variadic(&self) -> bool {
        self.templates
            .iter()
            .any(|t| t.is_variadic_param.iter().any(|&p| p))
            || self.partials.iter().any(|p| p.pattern_text.contains("..."))
    }

    /// G12: resolve args when variadic packs are involved.
    ///
    /// M52: 已废弃 —— 包形参的吃实参规则并入 `resolve_args` (真正的根因位置:
    /// 它之前无条件 `args.to_vec()` 旁路了默认实参填充与 `requires` 求值, 并以
    /// `has_variadic()` 这个**全局开关**影响所有模板, 一个变参声明就会把整仓库
    /// 的实参解析降级到那条简化路径).
    #[deprecated(note = "use resolve_args, it is pack-aware now")]
    pub fn resolve_args_with_variadic(
        &self,
        td: &TemplateDecl,
        args: &[MangleType],
    ) -> Option<Vec<MangleType>> {
        self.resolve_args(td, args)
    }

    /// 注册模板声明 (重复注册同一模板: 后者覆盖, 便于 stdlib 头重复 include)
    ///
    /// M51: 只对"完全同一模板"去重 (名称 + 形参表 + requires + 函数签名),
    /// 同名但签名不同的**重载函数模板**必须共存, 否则隐式推导时只剩一个候选,
    /// 无法实现"替换失败的候选被静默剔除、其余候选仍可选中"的 SFINAE 语义.
    pub fn register(&mut self, targs: &TemplateArgs, requires: &Option<String>, decl: &CppDecl) {
        if let Some(td) = Self::decl_from(targs, requires, decl.clone()) {
            let fp = td.fingerprint();
            self.templates.retain(|t| t.fingerprint() != fp);
            self.templates.push(td);
        }
    }

    /// 注册偏特化
    pub fn register_partial(
        &mut self,
        name: String,
        params: &[String],
        pattern_text: String,
        decl: &CppDecl,
    ) {
        self.partials.push(PartialSpecEntry {
            name,
            param_names: params.to_vec(),
            pattern_text,
            decl: decl.clone(),
        });
    }

    /// G13: 注册全特化
    pub fn register_full(
        &mut self,
        name: String,
        specific_args: Vec<MangleType>,
        body: &crate::cpp::parser::CppBody,
    ) {
        self.full_specs.push(FullSpecEntry {
            name,
            specific_args,
            body: body.clone(),
        });
    }

    /// 查找模板 (按名)
    pub fn find(&self, name: &str) -> Option<&TemplateDecl> {
        self.templates.iter().find(|t| t.name == name)
    }

    /// `name<args>` **已实例化**后的类名; 未实例化则 None.
    ///
    /// M52: 显式实参可以省略带默认值的尾部形参
    /// (`template <class K, class V, class C = int>` 用 `fm<int, int>`),
    /// 而实例缓存与实例类名都以 `resolve_args` 补全后的实参为准. 直接拿调用点
    /// 实参算 `instantiated_class_name` 会得到少一段的
    /// `__dcc_template_fm_int_int`, 与真正登记的 `__dcc_template_fm_int_int_int`
    /// 不是同一个符号 —— 类型替换静默不发生, 最后在 sema 变成
    /// `unknown class type`. 本函数以**缓存里的实例声明名**为唯一真值.
    pub fn instance_class_name(&self, name: &str, args: &[MangleType]) -> Option<String> {
        let mut keys = vec![Self::key_for(name, args)];
        if let Some(td) = self.find(name) {
            if let Some(resolved) = self.resolve_args(td, args) {
                let rk = Self::key_for(name, &resolved);
                if !keys.contains(&rk) {
                    keys.push(rk);
                }
            }
        }
        for k in keys {
            if let Some(CppDecl::Class(c)) = self.inst_cache.get(&k) {
                return Some(c.name.clone());
            }
        }
        None
    }

    /// M51: 按名取**全部**候选模板 (重载集) 及其在主表中的下标;
    /// 下标是同一 name+args 不同候选的区分符 (见 `fn_inst_key`).
    pub fn candidates_named(&self, name: &str) -> Vec<(usize, &TemplateDecl)> {
        self.templates
            .iter()
            .enumerate()
            .filter(|(_, t)| t.name == name)
            .collect()
    }

    /// M51: 是否存在某个函数/类模板注册表项可作为隐式推导候选
    pub fn has_template_named(&self, name: &str) -> bool {
        self.templates.iter().any(|t| t.name == name)
    }

    /// M51: 该名字是否**只**由函数模板承载 (至少一个函数模板候选, 且无类模板同名).
    ///
    /// 用于"无匹配函数"硬报错的门控: 类模板名出现在调用位置
    /// (`std::vector<int>(other)` 这种临时量构造 / 函数式类型转换) 不是
    /// 函数调用, 按候选决议失败报错会把合法代码拒之门外.
    pub fn only_fn_templates_named(&self, name: &str) -> bool {
        let mut saw_fn = false;
        for t in &self.templates {
            if t.name != name {
                continue;
            }
            match &t.decl {
                CppDecl::FreeFn { .. } => saw_fn = true,
                _ => return false,
            }
        }
        saw_fn
    }

    /// M51: 注册 C++20 概念 `concept C = constraint;`
    /// `params` 是包裹它的 `template <typename T, ...>` 形参名列表.
    pub fn register_concept(&mut self, name: &str, params: &[String], body: &str) {
        self.concepts.insert(name.to_string(), body.to_string());
        self.concept_params.insert(name.to_string(), params.to_vec());
    }

    /// M51: 记录模板/概念定义锚点 (取首次出现的顶层位置)
    pub fn set_anchor(&mut self, name: &str, anchor: usize) {
        self.anchors.entry(name.to_string()).or_insert(anchor);
    }

    /// M51: 查询定义锚点 (未登记时返回 usize::MAX, 由调用方按"追加到末尾"处理)
    pub fn anchor_of(&self, name: &str) -> usize {
        self.anchors.get(name).copied().unwrap_or(usize::MAX)
    }

    /// M51: 展开约束文本中的概念引用 `C<args...>` → `(概念体[形参:=实参])`.
    ///
    /// 与 `feature_degrade::expand_concepts` 的"只认单形参且固定叫 T"不同,
    /// 这里按概念自己的形参名逐个绑定, 因此
    ///   - 多形参概念 (`same_as<T, U>`) 能正确替换两个形参;
    ///   - 概念形参与外层模板形参同名/不同名都不会串味;
    ///   - 概念嵌套引用概念会在下一轮循环里继续展开 (上限 8 层防环).
    pub fn expand_concept_refs(&self, cond: &str) -> String {
        let mut cur = crate::cpp::feature_degrade::normalize_constraint(cond);
        for _ in 0..8 {
            let Some((start, lt, close, name)) = self.find_concept_ref(&cur) else {
                break;
            };
            // M51: 实参区间位于左尖括号与右尖括号之间, 而不是 名字起点+1 .
            // Small<T> 的名字占 5 个字符, 用 start+1 会切出 mall<T 这种垃圾,
            // 概念形参随即被绑成错误文本, 展开出的约束体整个失真.
            let args_txt: Vec<String> = split_template_args(&cur[lt + 1..close])
                .into_iter()
                .map(|s| s.trim().to_string())
                .collect();
            let body = &self.concepts[&name];
            let mut ctx = crate::cpp::feature_degrade::SfinaeCtx::new();
            if let Some(params) = self.concept_params.get(&name) {
                for (p, a) in params.iter().zip(args_txt.iter()) {
                    ctx.bind(p.clone(), a.clone());
                }
            }
            let expanded = crate::cpp::feature_degrade::try_substitute(body, &ctx)
                .unwrap_or_else(|_| body.clone());
            cur.replace_range(start..=close, &format!("({})", expanded));
        }
        crate::cpp::feature_degrade::normalize_constraint(&cur)
    }

    /// 在 (已规范化的) 约束文本里找第一个概念引用:
    /// 返回 (名字起点, 左尖括号位置, 右尖括号位置, 概念名).
    ///
    /// 三个下标语义必须和调用方对齐: 名字区间 [start, lt), 实参区间 (lt, close).
    fn find_concept_ref(&self, text: &str) -> Option<(usize, usize, usize, String)> {
        let bytes = text.as_bytes();
        let is_ident_start = |b: u8| b.is_ascii_alphabetic() || b == b'_';
        let is_ident_cont = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut i = 0usize;
        while i < bytes.len() {
            if !is_ident_start(bytes[i]) || (i > 0 && (is_ident_cont(bytes[i - 1]) || bytes[i - 1] == b':'))
            {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < bytes.len() && is_ident_cont(bytes[j]) {
                j += 1;
            }
            // 允许限定名 `std::Foo<...>`
            let mut name_end = j;
            while name_end + 1 < bytes.len() && &bytes[name_end..name_end + 2] == b"::" {
                let mut k = name_end + 2;
                while k < bytes.len() && is_ident_cont(bytes[k]) {
                    k += 1;
                }
                name_end = k;
            }
            if name_end < bytes.len() && bytes[name_end] == b'<' {
                let raw = text.get(i..name_end).unwrap_or("");
                // 依次按全名 / 去限定符短名查概念表 (parser 常把 `std::C` 降为 `C`)
                let key = if self.concepts.contains_key(raw) {
                    Some(raw.to_string())
                } else {
                    let short = raw.rsplit("::").next().unwrap_or(raw).to_string();
                    if self.concepts.contains_key(&short) {
                        Some(short)
                    } else {
                        None
                    }
                };
                if let Some(name) = key {
                    if let Some(close) = find_angle_end(bytes, name_end) {
                        return Some((i, name_end, close, name));
                    }
                }
            }
            i = name_end.max(j);
        }
        None
    }

    /// M51: 求值 `requires` 约束表达式.
    ///
    /// 先展开概念引用 (`Integral<T>` → 概念体), 再交给 trait 求值器;
    /// 返回 Some(false) 表示约束不满足 —— 调用方必须按 SFINAE 静默剔除该
    /// 候选, 而不是当硬错误; None 表示无法判定 (保守放行).
    pub fn evaluate_constraint(&self, cond: &str, ctx: &SfinaeCtx) -> Option<bool> {
        let expanded = self.expand_concept_refs(cond);
        if expanded.trim().is_empty() {
            return None;
        }
        crate::cpp::feature_degrade::evaluate_trait_condition(&expanded, ctx)
    }

    /// 实例化 key: `name<type_key(arg0),...>`
    pub fn key_for(name: &str, args: &[MangleType]) -> String {
        let mut s = format!("{}<", name);
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&type_key(a));
        }
        s.push('>');
        s
    }

    /// 解析模板实参 (默认实参填充 + requires 约束检查)
    /// 失败返回 None (约束不满足 / 实参个数不足) → SFINAE 跳过
    pub fn resolve_args(&self, td: &TemplateDecl, args: &[MangleType]) -> Option<Vec<MangleType>> {
        let mut resolved: Vec<MangleType> = Vec::new();
        // G12/M52: 包形参**吃掉多个实参** (`foo<int, double>` 对一个
        // `typename... T` 形参是 2 个实参 1 个形参), 故按形参逐个消耗实参游标,
        // 不能用 `args.get(i)` 按下标对齐 (那会把包后面的实参全部弄丢).
        let mut ai = 0usize;
        for (i, _p) in td.param_names.iter().enumerate() {
            let is_pack = td.is_variadic_param.get(i).copied().unwrap_or(false);
            if is_pack {
                // 包后面的非包形参还得给他们留够实参 (zip 规则的最小实现)
                let tail = td.param_names.len() - i - 1;
                let end = args.len().saturating_sub(tail);
                if end < ai {
                    return None; // 实参不足以覆盖后面的形参
                }
                let packed = if end == ai + 1
                    && matches!(args.get(ai), Some(MangleType::VariadicPack(_)))
                {
                    // 已经是一个包 (如 `Types...` 模板实参 / 隐式推导结果): 直接吃下
                    args[ai].clone()
                } else {
                    MangleType::VariadicPack(args[ai..end].to_vec())
                };
                resolved.push(packed);
                ai = end;
                continue;
            }
            if let Some(a) = args.get(ai) {
                resolved.push(a.clone());
                ai += 1;
            } else if let Some(d) = td.defaults.get(i).cloned().flatten() {
                resolved.push(d);
            } else if i == 0 && args.is_empty() && !td.defaults.is_empty() {
                // 单默认形参模板 `template <typename T = int>` 且无实参
                resolved.push(td.defaults[0].clone().unwrap());
            } else {
                return None; // 实参不足且无默认值
            }
        }
        if ai != args.len() {
            // 实参多于形参: 不是本模板的合法调用 (SFINAE 剔除该候选)
            return None;
        }
        let _ = td;
        // requires 约束求值 (概念过滤): 不满足 → 替换失败 → SFINAE 剔除候选
        if let Some(cond) = &td.requires {
            let mut ctx = crate::cpp::feature_degrade::SfinaeCtx::new();
            for (pn, pa) in td.param_names.iter().zip(resolved.iter()) {
                ctx.bind(pn.clone(), type_key(pa));
            }
            let verdict = self.evaluate_constraint(cond, &ctx);
            if std::env::var_os("DCC_TPL_DEBUG").is_some() {
                eprintln!(
                    "[tpl] requires {:?} 于 {}<{:?}> => {:?}",
                    cond,
                    td.name,
                    resolved.iter().map(type_key).collect::<Vec<_>>(),
                    verdict
                );
            }
            if !verdict.unwrap_or(true) {
                return None;
            }
        }
        Some(resolved)
    }

    /// 从调用实参类型推导模板实参 (隐式实例化)
    /// call_arg_tys: 调用点实参的 MangleType (由 lower 计算)
    ///
    /// 遵循 C++ `[temp.deduct.call]` 的关键子集:
    ///   1. 实参个数必须与函数形参个数一致 (否则不是本模板的合法调用);
    ///   2. 同一形参从不同实参推出不同结果 → 推导失败 (SFINAE 剔除候选);
    ///   3. 推不出的形参用默认实参兜底; 既推不出又无默认 → 失败;
    ///   4. 最后交给 `resolve_args` 填充默认 + 求值 `requires` (不满足 → None).
    pub fn deduce_args(
        &self,
        td: &TemplateDecl,
        call_arg_tys: &[MangleType],
    ) -> Option<Vec<MangleType>> {
        // 从函数参数类型推导: 形参类型中出现的形参名 ← 实参类型
        let fn_params: Vec<MangleType> = match &td.decl {
            CppDecl::FreeFn { params, .. } => params.iter().map(|(_, t)| t.clone()).collect(),
            _ => return None,
        };
        if fn_params.len() != call_arg_tys.len()
            && !fn_params
                .iter()
                .any(|t| matches!(t, MangleType::VariadicPack(_)))
        {
            return None;
        }
        // G12/M52: 形参逐个消耗实参游标; `Args... args` 形态的包形参吃掉
        // 它后面的所有实参 (除留给后面非包形参的那些).
        let mut bindings: Vec<(String, MangleType)> = Vec::new();
        let mut pi = 0usize;
        let mut ci = 0usize;
        while pi < fn_params.len() {
            if let MangleType::VariadicPack(pat) = &fn_params[pi] {
                let tail = fn_params.len() - pi - 1;
                let end = call_arg_tys.len().saturating_sub(tail);
                if end < ci {
                    return None;
                }
                bind_pack(pat, &call_arg_tys[ci..end], &td.param_names, &mut bindings);
                ci = end;
            } else {
                if ci >= call_arg_tys.len() {
                    return None;
                }
                collect_bindings(&fn_params[pi], &call_arg_tys[ci], &td.param_names, &mut bindings);
                ci += 1;
            }
            pi += 1;
        }
        if ci != call_arg_tys.len() {
            // 实参多于形参 (且没有包形参吃掉他们): 不是本模板的合法调用
            return None;
        }
        // 生成实参 (按形参顺序; 未推导出的用默认或失败)
        let mut args: Vec<MangleType> = Vec::new();
        for (i, pn) in td.param_names.iter().enumerate() {
            match Self::deduced_for(&bindings, pn) {
                Some(a) => args.push(a),
                None => {
                    // G12: 包形参未被推导 = **空包** (`f()` 对 `A... a` 合法),
                    // 不能当成"推不出"而剔除候选.
                    if td.is_variadic_param.get(i).copied().unwrap_or(false) {
                        args.push(MangleType::VariadicPack(Vec::new()));
                        continue;
                    }
                    let d = td.defaults.get(i).cloned().flatten()?;
                    args.push(d);
                }
            }
        }
        self.resolve_args(td, &args)
    }

    /// 取某个形参的推导结果; 多处以不同结果被绑定则返回 None (推导冲突).
    fn deduced_for(bindings: &[(String, MangleType)], name: &str) -> Option<MangleType> {
        let mut found: Option<MangleType> = None;
        for (b, a) in bindings {
            if b != name {
                continue;
            }
            match &found {
                None => found = Some(a.clone()),
                Some(prev) if prev == a => {}
                Some(_) => return None,
            }
        }
        found
    }

    /// M51: 显式模板实参规范化: 填充默认实参 + 求值 `requires`.
    /// 返回 `(候选下标, 实参表)`; None 表示该名字下所有候选都不可行
    /// (约束不满足 / 实参过多或不足且无默认), 调用方必须按 SFINAE 静默跳过,
    /// 不得报硬错误.
    pub fn normalize_args(&self, name: &str, args: &[MangleType]) -> Option<(usize, Vec<MangleType>)> {
        self.candidates_named(name)
            .into_iter()
            .find_map(|(i, td)| self.resolve_args(td, args).map(|r| (i, r)))
    }

    /// M51: 函数模板实例的缓存 key: **必须带候选下标**.
    ///
    /// 同一名字的多个重载候选可能推出一字不差的实参表 (如
    /// `template<Small<T>> int tag(T)` 与 `template<> int tag(T)`, 两者都推
    /// 出 `T=int`), 只用 `name<args>` 做键会让第二个候选被第一个的缓存命中
    /// “冒充”, 重载选择结果错误 (g++ 交叉验证时发现).
    pub fn fn_inst_key(idx: usize, name: &str, args: &[MangleType]) -> String {
        format!("#{}:{}", idx, Self::key_for(name, args))
    }

    /// M51: 查 `name<args>` 中**已实例化过**的首个候选下标 (供后面的
    /// 名字重写阶段反查, 它手上只有名字+实参, 没有推导时的候选信息).
    pub fn cached_fn_candidate(&self, name: &str, args: &[MangleType]) -> Option<usize> {
        self.candidates_named(name)
            .into_iter()
            .map(|(i, _)| i)
            .find(|i| self.inst_cache.contains_key(&Self::fn_inst_key(*i, name, args)))
    }

    /// 实例化产物的统一收尾: **参数包摊平** (见 `pack_expand` 模块).
    ///
    /// 失败时保留原树但记下原因 (`pack_expand_error`), 由调用端 (native 后端 /
    /// lower) 硬报错 —— 静默交出去就会在背深处变成莫名的符号错误.
    fn finish_inst(&mut self, mut inst: CppDecl, params: &[String], args: &[MangleType]) -> CppDecl {
        if let Err(msg) = crate::cpp::pack_expand::expand_packs_in(&mut inst, params, args) {
            if self.pack_expand_error.is_none() {
                self.pack_expand_error = Some(msg);
            }
        }
        inst
    }

    /// M51: 实例化**指定候选**的函数模板 (重载集里的那个下标).
    /// 与 `instantiate_fn` 的区别: 不重新选候选, 保证调用点重写出的 C 名
    /// (`tplfn_c_name(name, idx, args)`) 与这里产出的符号一致.
    pub fn instantiate_fn_at(&mut self, idx: usize, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        let td = self.templates.get(idx)?.clone();
        if td.name != name {
            return None;
        }
        let resolved = self.resolve_args(&td, args)?;
        let key = Self::fn_inst_key(idx, name, &resolved);
        if let Some(cached) = self.inst_cache.get(&key) {
            return Some(cached.clone());
        }
        let primary_params = td.template_params.clone();
        let inst = instantiate_decl_with_template_params(
            &td.decl,
            &td.param_names,
            &resolved,
            None,
            &primary_params,
        );
        let inst = self.finish_inst(inst, &td.param_names, &resolved);
        self.inst_cache.insert(key.clone(), inst.clone());
        self.inst_order.push(key);
        Some(inst)
    }

    /// M51: 对 `name` 的全部重载候选逐个做隐式实参推导, 返回首个成功的
    /// `(候选下标, 实参表)`.
    ///
    /// 这就是 SFINAE 的候选集语义: 某个候选推导失败 / `requires` 不满足 /
    /// 推导冲突, 都**静默**跳过 (不报硬错误), 只有全部候选都失败才返回 None.
    pub fn deduce_for_call(
        &self,
        name: &str,
        call_arg_tys: &[MangleType],
    ) -> Option<(usize, Vec<MangleType>)> {
        self.templates
            .iter()
            .enumerate()
            .filter(|(_, t)| t.name == name)
            .find_map(|(i, td)| self.deduce_args(td, call_arg_tys).map(|r| (i, r)))
    }

    /// G13: 实例化模板类 - 优先匹配全特化 > 偏特化 > 主模板
    pub fn instantiate_class(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        // First try full specialization (exact match)
        if let Some(full) = self.find_full_spec(name, args) {
            // Full spec is already concrete, no instantiation needed
            // Convert body to a class declaration
            return Some(self.full_body_to_class_decl(&full.name, &full.body));
        }

        // Try partial specialization
        if let Some((decl, param_names, bound)) = self.pick_partial(name, args) {
            let key = Self::key_for(name, args);
            if let Some(cached) = self.inst_cache.get(&key) {
                return Some(cached.clone());
            }
            let inst = instantiate_decl(&decl, &param_names, &bound, Some(name), args);
            let inst = self.finish_inst(inst, &param_names, &bound);
            self.inst_cache.insert(key.clone(), inst.clone());
            self.inst_order.push(key);
            return Some(inst);
        }

        // Fallback to primary template
        let td = self.find(name).cloned()?;
        let resolved = self.resolve_args(&td, args)?;
        let key = Self::key_for(name, &resolved);
        if let Some(cached) = self.inst_cache.get(&key) {
            return Some(cached.clone());
        }
        // G11: 使用模板模板参数增强的实例化
        let primary_params = &td.template_params;
        // 类实例化必须使用实例化后的名称 (如 __dcc_template_Box_int)
        let inst_name = instantiated_class_name(name, &resolved);
        let inst = instantiate_decl_with_template_params(
            &td.decl,
            &td.param_names,
            &resolved,
            Some(&inst_name),
            primary_params,
        );
        let inst = self.finish_inst(inst, &td.param_names, &resolved);
        self.inst_cache.insert(key.clone(), inst.clone());
        self.inst_order.push(key);
        Some(inst)
    }

    /// 实例化模板函数 `name<args>`; 返回实例化后的 FreeFn 声明
    ///
    /// M51: 同名重载模板逐个尝试 `resolve_args` (填充默认 + 求值 requires),
    /// 某个候选替换失败时不报错, 静默尝试下一个 (SFINAE 候选集语义).
    pub fn instantiate_fn(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        let candidates: Vec<usize> = self
            .templates
            .iter()
            .enumerate()
            .filter(|(_, t)| t.name == name)
            .map(|(i, _)| i)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // 先选定能完成实参解析的那个候选 (及其解析后的实参)
        let mut chosen: Option<(usize, Vec<MangleType>)> = None;
        for &idx in &candidates {
            let td = &self.templates[idx];
            let resolved = self.resolve_args(td, args);
            if let Some(r) = resolved {
                chosen = Some((idx, r));
                break;
            }
        }
        let (idx, resolved) = chosen?;
        let key = Self::key_for(name, &resolved);
        if let Some(cached) = self.inst_cache.get(&key) {
            return Some(cached.clone());
        }
        // G11: 使用模板模板参数增强的实例化
        let primary_params = self.templates[idx].template_params.clone();
        let tpl_params = self.templates[idx].param_names.clone();
        let inst = instantiate_decl_with_template_params(
            &self.templates[idx].decl,
            &self.templates[idx].param_names,
            &resolved,
            None,
            &primary_params,
        );
        let inst = self.finish_inst(inst, &tpl_params, &resolved);
        self.inst_cache.insert(key.clone(), inst.clone());
        self.inst_order.push(key);
        Some(inst)
    }

    /// 选择偏特化: 若实参匹配某偏特化模式, 返回 (特化声明, 形参名, 实参)
    /// M27: 实现 GCC/Clang 风格的 specialization 排序:
    /// 多个偏特化匹配时, 选最特化 (most specialized) 的那个.
    /// 特化度评分: 精确匹配 > 指针/引用模式匹配 > 通配符匹配.
    pub fn pick_partial(
        &self,
        name: &str,
        args: &[MangleType],
    ) -> Option<(CppDecl, Vec<String>, Vec<MangleType>)> {
        let arg_keys: Vec<String> = args.iter().map(type_key).collect();
        let mut candidates: Vec<(usize, &PartialSpecEntry)> = Vec::new();
        // M30.1: 每个候选记录其形参绑定 (形参名 → 实参类型),
        // 偏特化形参表与主模板不同 (如 `conditional<false, T, F>` 声明
        // <typename T, typename F> 但 pattern 占 3 个主实参位),
        // 返回时按偏特化形参名对齐, 不能直接用主实参列表
        let mut candidate_binds: Vec<Vec<(String, MangleType)>> = Vec::new();
        for p in &self.partials {
            if p.name != name {
                continue;
            }
            let patterns: Vec<&str> = p.pattern_text.split(',').collect();
            if patterns.len() != arg_keys.len() {
                continue;
            }
            // 计算匹配度和特化度分数
            let mut specificity: usize = 0;
            let mut all_match = true;
            // M29: 同一形参名在 pattern 中多次出现时, 绑定必须一致
            // (如 `is_same<T, T>` 对 [int, long] 不匹配, 对 [int, int] 匹配)
            let mut bindings: Vec<(String, MangleType)> = Vec::new();
            for (pat, (arg, arg_ty)) in patterns.iter().zip(arg_keys.iter().zip(args.iter())) {
                let pat = pat.trim();
                let pat_key = pat.replace('*', "").trim().to_string();
                // 检查/记录形参绑定; 同名二次出现时实参必须一致
                // `arg_key` 是本次参与比较的实参键: 通配符分支部它就是整个
                // 实参的键, 指针分支部则是剔掉一层 `*` 后的键 (与绑定的
                // 形参类型保持一致, 否则同一个 `T` 出现两次时永不可能相等).
                let bind_check = |bindings: &mut Vec<(String, MangleType)>,
                                  name: &str,
                                  arg_ty: &MangleType,
                                  arg_key: &str,
                                  all_match: &mut bool| {
                    if let Some((_, prev)) = bindings.iter().find(|(n, _)| n == name) {
                        if type_key(prev) != arg_key {
                            *all_match = false;
                        }
                    } else {
                        bindings.push((name.to_string(), arg_ty.clone()));
                    }
                };
                if pat.ends_with('*') && arg.starts_with("p_") {
                    // 指针模式：`T*` 匹配 `p_<T>` 编码
                    if pat_key == "T" || arg.contains(&pat_key) {
                        specificity += 2; // 指针模式比通配符更特化
                        // 注意必须用**去掉 `*` 后**的 `pat_key` 判断形参名:
                        // pattern 文本是 `T*`, 形参表里是 `T`, 直接拿 `T*`
                        // 去比对永远不相等, 于是 `T` 根本没有绑定, 后面的
                        // `bound` 兜底逻辑会塞进**整个指针实参**, 使得
                        // `iterator_traits<T*>::value_type` 的 `T` 变成 `int*`
                        // (sizeof 从 4 变 8), 指针特化整体失效.
                        if Self::is_param_name_internal(&pat_key, &p.param_names) {
                            // 形参 `T` 绑的是**被指对象**, 不是整个指针:
                            // `iterator_traits<T*>` 里 `typedef T value_type` 对
                            // `int*` 必须得到 `int` (绑成 `int*` 则 sizeof 从 4
                            // 变成 8, 指针特化整体失效).
                            let (bound_ty, bound_key) = match arg_ty {
                                MangleType::Pointer(inner) => {
                                    ((**inner).clone(), arg.trim_start_matches("p_").to_string())
                                },
                                other => (other.clone(), arg.clone()),
                            };
                            bind_check(
                                &mut bindings,
                                &pat_key,
                                &bound_ty,
                                &bound_key,
                                &mut all_match,
                            );
                        }
                    } else {
                        all_match = false;
                        break;
                    }
                } else if pat_key.is_empty() {
                    all_match = false;
                    break;
                } else if pat_key == *arg {
                    // 精确匹配：最特化
                    specificity += 3;
                } else if let (Some(pv), Some(av)) =
                    (Self::parse_non_type_pattern(&pat_key), Self::parse_non_type_pattern(arg))
                {
                    // M30.1: 非类型实参匹配 (`conditional<false, T, F>` 的
                    // pattern `false` 与实参 NonType(0)). pattern 与实参
                    // key 都是编译期常量字面量，值相等才匹配.
                    if pv == av {
                        specificity += 3;
                    } else {
                        all_match = false;
                        break;
                    }
                } else if Self::is_param_name_internal(pat, &p.param_names) {
                    // 通配符: 匹配任意类型, 最不特化; 需绑定一致
                    bind_check(&mut bindings, &pat_key, arg_ty, arg, &mut all_match);
                    specificity += 1;
                } else {
                    all_match = false;
                    break;
                }
                if !all_match {
                    break;
                }
            }
            if all_match {
                candidates.push((specificity, p));
                candidate_binds.push(bindings);
            }
        }
        if candidates.is_empty() {
            return None;
        }
        // 选特化度最高的候选 (分数越大越特化)
        // 按 specificity 排序时同步取该候选的绑定表
        type Ranked<'a> = (usize, &'a PartialSpecEntry, Vec<(String, MangleType)>);
        let mut ranked: Vec<Ranked<'_>> = candidates
            .into_iter()
            .zip(candidate_binds)
            .map(|((s, p), b)| (s, p, b))
            .collect();
        ranked.sort_by_key(|r| std::cmp::Reverse(r.0));
        let (_, p, bindings) = &ranked[0];
        // M30.1: 按偏特化形参名对齐绑定值; pattern 中未出现的形参
        // (极少见) 用主实参同位置兜底, 保证 substitute 不越界
        let bound: Vec<MangleType> = p
            .param_names
            .iter()
            .enumerate()
            .map(|(i, pn)| {
                bindings
                    .iter()
                    .find(|(n, _)| n == pn)
                    .map(|(_, t)| t.clone())
                    .unwrap_or_else(|| args.get(i).cloned().unwrap_or(MangleType::Int))
            })
            .collect();
        Some((p.decl.clone(), p.param_names.clone(), bound))
    }

    /// G13: 查找全特化：精确匹配优先
    pub fn find_full_spec(&self, name: &str, args: &[MangleType]) -> Option<&FullSpecEntry> {
        self.full_specs.iter().find(|full| {
            if full.name != name {
                return false;
            }
            // Full specialization must match exactly
            if full.specific_args.len() != args.len() {
                return false;
            }
            for (spec_arg, req_arg) in full.specific_args.iter().zip(args.iter()) {
                if type_key(spec_arg) != type_key(req_arg) {
                    return false;
                }
            }
            true
        })
    }

    /// G13: Find the best matching instantiation with priority:
    /// 1. Full specializations (exact match required)
    /// 2. Partial specializations (most specialized)
    /// 3. Primary template (fallback)
    pub fn get_best_match(&self, name: &str, args: &[MangleType]) -> BestMatchResult<'_> {
        // First try full spec
        if let Some(full) = self.find_full_spec(name, args) {
            return BestMatchResult::FullSpec(full);
        }

        // Then try partial specs
        if let Some(partial) = self.pick_partial(name, args) {
            return BestMatchResult::PartialSpec(Box::new(partial));
        }

        // Finally fallback to primary template
        if let Some(prim) = self.find(name) {
            return BestMatchResult::Primary(prim);
        }

        BestMatchResult::NotFound
    }
}

impl TemplateRegistry {
    /// G13: Convert full specialization body to a class declaration
    fn full_body_to_class_decl(&self, name: &str, body: &CppBody) -> CppDecl {
        // For now, create a minimal struct - the body members will be properly handled in lowering
        let mut members = Vec::new();

        // Extract members from the full spec body if any
        for stmt in &body.stmts {
            if let CppStmt::Decl(decl) = stmt {
                match decl {
                    CppDecl::MemberVar {
                        ty,
                        name: member_name,
                        ..
                    } => {
                        members.push(CppDecl::MemberVar {
                            ty: ty.clone(),
                            name: member_name.clone(),
                            is_mutable: false,
                            is_volatile: false,
                        });
                    },
                    CppDecl::Typedef {
                        target,
                        name: alias,
                    } => {
                        members.push(CppDecl::Typedef {
                            target: target.clone(),
                            name: alias.clone(),
                        });
                    },
                    _ => {},
                }
            }
        }

        // Simple hash using String as key
        let hash_val = format!("{}", name.len() * 17);

        CppDecl::Class(ClassDecl {
            name: format!("__dcc_fullspec_{}_{}", name, hash_val),
            is_struct: true,
            is_union: false,
            bases: vec![],
            members,
            member_access: vec![],
        })
    }
}

///判断 pattern 文本是否是形参名 (通配符)
fn is_param_name(pat: &str, param_names: &[String]) -> bool {
    param_names.iter().any(|p| p == pat)
}

impl TemplateRegistry {
    fn is_param_name_internal(pat: &str, param_names: &[String]) -> bool {
        is_param_name(pat, param_names)
    }

    /// M30.1: Parse non-type pattern key from string.
    /// Accepts true/false and decimal/hex integers;
    /// returns None for non-NonType text.
    pub fn parse_non_type_pattern(s: &str) -> Option<i64> {
        match s {
            "true" => return Some(1),
            "false" => return Some(0),
            _ => {},
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            return i64::from_str_radix(hex, 16).ok();
        }
        if let Some(num) = s.strip_prefix('n') {
            return num.parse::<i64>().ok();
        }
        s.parse::<i64>().ok()
    }
}

/// 从形参/实参类型对中收集绑定 (形参名 → 实参类型)
/// 形如 `Class("T")` ↔ 实参 `Int`; `Pointer(Class("T"))` ↔ `Pointer(Int)` 递归
fn collect_bindings(
    pt: &MangleType,
    at: &MangleType,
    names: &[String],
    out: &mut Vec<(String, MangleType)>,
) {
    match (pt, at) {
        (MangleType::Class(n), _) if names.iter().any(|x| x == n) => {
            // 形参名直接对应实参 (整棵)
            out.push((n.clone(), at.clone()));
        },
        (MangleType::Pointer(a), MangleType::Pointer(b)) => collect_bindings(a, b, names, out),
        (MangleType::Template { args: pa, .. }, MangleType::Template { args: aa, .. }) => {
            for (x, y) in pa.iter().zip(aa.iter()) {
                collect_bindings(x, y, names, out);
            }
        },
        // 引用形参: 按 [temp.deduct.call]/1 先剥掉参考再推 (`const T&` ← `int` 值实参)
        (MangleType::Reference(a), MangleType::Reference(b)) => collect_bindings(a, b, names, out),
        (MangleType::Reference(a), _) => collect_bindings(a, at, names, out),
        _ => {},
    }
}

/// G12/M52: 从**包形参**的 N 个实参推导出包模板形参.
///
/// `template <typename... A> void f(A... a)` 以 `(int, double)` 调用:
/// 模式 `A` 对每个实参单独推导, 同名结果按实参顺序归并成
/// `A = VariadicPack([int, double])`. 多包同模式 (`pair<A,B>...`) 同样成立:
/// 每个名字各自归并, 长度不一致时由 `pack_expand` 按 zip 规则硬报错.
fn bind_pack(
    pat: &[MangleType],
    pack_tys: &[MangleType],
    names: &[String],
    out: &mut Vec<(String, MangleType)>,
) {
    for elem in pat {
        let mut per: Vec<(String, MangleType)> = Vec::new();
        for at in pack_tys {
            collect_bindings(elem, at, names, &mut per);
        }
        let mut seen: Vec<String> = Vec::new();
        for (n, _) in &per {
            if !seen.iter().any(|s| s == n) {
                seen.push(n.clone());
            }
        }
        for n in seen {
            let tys: Vec<MangleType> = per
                .iter()
                .filter(|(bn, _)| bn == &n)
                .map(|(_, t)| t.clone())
                .collect();
            out.push((n, MangleType::VariadicPack(tys)));
        }
    }
}

// ===== AST 级替换 =====

/// 把类型中的模板形参替换为实参
pub fn substitute_type(ty: &MangleType, params: &[String], args: &[MangleType]) -> MangleType {
    substitute_type_ex(ty, params, args, false)
}

/// G12/M52: `in_pattern` = 当前正处在一个**展开模式** (`VariadicPack` 的 payload)
/// 内部. 该语境下引用**包形参**的名字必须原样保留 —— 它是 [`pack_expand`]
/// 后面要 zip 的那个包; 若在这里就换成整条实参表, 模式会失去包身份
/// (`A... args` 变成 `{int,double}... args`), 摊平就无从下手.
/// 绑定到**非包**实参的形参在两种语境下都照常替换.
fn substitute_type_ex(
    ty: &MangleType,
    params: &[String],
    args: &[MangleType],
    in_pattern: bool,
) -> MangleType {
    let sub = |t: &MangleType| substitute_type_ex(t, params, args, in_pattern);
    match ty {
        MangleType::Class(n) => {
            if let Some(idx) = params.iter().position(|p| p == n) {
                if let Some(a) = args.get(idx) {
                    if in_pattern && matches!(a, MangleType::VariadicPack(_)) {
                        return ty.clone();
                    }
                    return a.clone();
                }
            }
            ty.clone()
        },
        // G12/M52: 包类型本身 recurse 到元素 (之前由下面的 `other => clone()`
        // 直接原样返回, 导致 `A...` 里的 `A` 永远替换不掉), 并进入模式语境.
        MangleType::VariadicPack(packed) => MangleType::VariadicPack(
            packed
                .iter()
                .map(|t| substitute_type_ex(t, params, args, true))
                .collect(),
        ),
        MangleType::Pointer(t) => MangleType::Pointer(Box::new(sub(t))),
        MangleType::Reference(t) => MangleType::Reference(Box::new(sub(t))),
        MangleType::Array(t, n) => MangleType::Array(Box::new(sub(t)), *n),
        MangleType::Template { name, args: tas } => MangleType::Template {
            name: name.clone(),
            args: tas.iter().map(sub).collect(),
        },
        // M30: `Tpl<Args>::member` 的实参递归替换; name/member 保留
        // (typedef 展开由 lower 在查 inst_typedefs 时完成)
        MangleType::TplMember {
            name,
            args: tas,
            member,
        } => MangleType::TplMember {
            name: name.clone(),
            args: tas.iter().map(sub).collect(),
            member: member.clone(),
        },
        other => other.clone(),
    }
}

/// Re-parse substituted declaration text and perform semantic analysis for dependent names
/// This implements the G14 Route A approach: keep text substitution but add semantic pass on top
pub fn substitute_and_resolve(
    decl: &CppDecl,
    params: &[String],
    args: &[MangleType],
    class_rename: Option<&str>,
) -> Result<CppDecl, String> {
    // Step 1: Text substitution (existing behavior)
    let substituted = substitute_decl(decl, params, args, class_rename);

    // G14 TODO: Future enhancement - re-parse and run semantic analysis:
    // 2. Serialize substituted AST back to text
    // 3. Re-parse into fresh AST
    // 4. Run semantic analysis with instantiation context
    // 5. Resolve dependent names using actual template arguments
    // For now, maintain backward compatibility by returning text-substituted result
    Ok(substituted)
}

/// 递归替换表达式内出现的模板形参类型
pub fn substitute_expr(e: &CppExpr, params: &[String], args: &[MangleType]) -> CppExpr {
    match e {
        CppExpr::Cast(ty, inner) => CppExpr::Cast(
            substitute_type(ty, params, args),
            Box::new(substitute_expr(inner, params, args)),
        ),
        CppExpr::StaticCast { ty, expr } => CppExpr::StaticCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::ConstCast { ty, expr } => CppExpr::ConstCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::ReinterpretCast { ty, expr } => CppExpr::ReinterpretCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::DynamicCast { ty, expr } => CppExpr::DynamicCast {
            ty: substitute_type(ty, params, args),
            expr: Box::new(substitute_expr(expr, params, args)),
        },
        CppExpr::TypeidType(ty) => CppExpr::TypeidType(substitute_type(ty, params, args)),
        CppExpr::Sizeof(SizeofArg::Type(ty)) => {
            CppExpr::Sizeof(SizeofArg::Type(substitute_type(ty, params, args)))
        },
        CppExpr::Sizeof(SizeofArg::Expr(x)) => {
            CppExpr::Sizeof(SizeofArg::Expr(Box::new(substitute_expr(x, params, args))))
        },
        CppExpr::TypedInitList { ty, elems } => CppExpr::TypedInitList {
            ty: substitute_type(ty, params, args),
            elems: elems
                .iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        },
        CppExpr::Member(o, s) => {
            CppExpr::Member(Box::new(substitute_expr(o, params, args)), s.clone())
        },
        CppExpr::PtrMember(o, s) => {
            CppExpr::PtrMember(Box::new(substitute_expr(o, params, args)), s.clone())
        },
        CppExpr::MemberStar(o, s) => {
            CppExpr::MemberStar(Box::new(substitute_expr(o, params, args)), s.clone())
        },
        CppExpr::PtrMemberStar(o, s) => {
            CppExpr::PtrMemberStar(Box::new(substitute_expr(o, params, args)), s.clone())
        },
        CppExpr::Call(f, as_) => {
            // 模板参数名在表达式语境中作为类型使用时 (如 `T()`、`new T`),
            // 替换为实际类型名或零值初始化.
            if let CppExpr::Ident(name) = f.as_ref() {
                if let Some(idx) = params.iter().position(|p| p == name) {
                    if let Some(a) = args.get(idx) {
                        match a {
                            MangleType::Class(n) => {
                                return CppExpr::Call(
                                    Box::new(CppExpr::Ident(n.clone())),
                                    as_.iter()
                                        .map(|x| substitute_expr(x, params, args))
                                        .collect(),
                                );
                            },
                            // 非类类型的函数式类型转换: `T()` 值初始化为 0;
                            // `T(x)` 是 functional cast, 必须保留实参值 (Cast 到 T),
                            // 不能把实参丢弃成 0。
                            _ => {
                                return match as_.as_slice() {
                                    [] => CppExpr::IntLit(0),
                                    _ => CppExpr::Cast(
                                        a.clone(),
                                        Box::new(substitute_expr(&as_[0], params, args)),
                                    ),
                                };
                            },
                        }
                    }
                }
            }
            CppExpr::Call(
                Box::new(substitute_expr(f, params, args)),
                as_.iter()
                    .map(|a| substitute_expr(a, params, args))
                    .collect(),
            )
        },
        CppExpr::New(ty, as_) => CppExpr::New(
            match substitute_type(&MangleType::Class(ty.clone()), params, args) {
                MangleType::Class(n) => n,
                other => type_key(&other),
            },
            as_.iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        ),
        CppExpr::NewArr { ty, count } => CppExpr::NewArr {
            ty: match substitute_type(&MangleType::Class(ty.clone()), params, args) {
                MangleType::Class(n) => n,
                other => type_key(&other),
            },
            count: Box::new(substitute_expr(count, params, args)),
        },
        CppExpr::PlacementNew { ptr, ty, args: as_ } => CppExpr::PlacementNew {
            ptr: Box::new(substitute_expr(ptr, params, args)),
            ty: match substitute_type(&MangleType::Class(ty.clone()), params, args) {
                MangleType::Class(n) => n,
                other => type_key(&other),
            },
            args: as_
                .iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        },
        CppExpr::BinOp(op, l, r) => CppExpr::BinOp(
            *op,
            Box::new(substitute_expr(l, params, args)),
            Box::new(substitute_expr(r, params, args)),
        ),
        CppExpr::UnaryOp(op, x) => {
            CppExpr::UnaryOp(*op, Box::new(substitute_expr(x, params, args)))
        },
        CppExpr::Ref(x) => CppExpr::Ref(Box::new(substitute_expr(x, params, args))),
        CppExpr::AddrOf(x) => CppExpr::AddrOf(Box::new(substitute_expr(x, params, args))),
        CppExpr::Delete(x) => CppExpr::Delete(Box::new(substitute_expr(x, params, args))),
        CppExpr::DeleteArr(x) => CppExpr::DeleteArr(Box::new(substitute_expr(x, params, args))),
        CppExpr::Noexcept(x) => CppExpr::Noexcept(Box::new(substitute_expr(x, params, args))),
        CppExpr::Typeid(x) => CppExpr::Typeid(Box::new(substitute_expr(x, params, args))),
        CppExpr::MultiIndex(o, idx) => CppExpr::MultiIndex(
            Box::new(substitute_expr(o, params, args)),
            idx.iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        ),
        CppExpr::Ternary { cond, then, els } => CppExpr::Ternary {
            cond: Box::new(substitute_expr(cond, params, args)),
            then: Box::new(substitute_expr(then, params, args)),
            els: Box::new(substitute_expr(els, params, args)),
        },
        CppExpr::Comma(xs) => CppExpr::Comma(
            xs.iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        ),
        CppExpr::InitList(xs) => CppExpr::InitList(
            xs.iter()
                .map(|x| substitute_expr(x, params, args))
                .collect(),
        ),
        CppExpr::OpCall { op, args: as_ } => CppExpr::OpCall {
            op: op.clone(),
            args: as_
                .iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        },
        CppExpr::Lambda(lam) => CppExpr::Lambda(crate::cpp::parser::LambdaExpr {
            captures: lam.captures.clone(),
            params: lam
                .params
                .iter()
                .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                .collect(),
            body: substitute_body(&lam.body, params, args),
            closure_name: lam.closure_name.clone(),
        }),
        // 模板**类型**参数在值语境中以类型名使用时 (如 `new T` 的类型段、
        // 类型位置引用) 替换为实际类名。非类型形参 (`template<int N>` 里的
        // `N`) 在值语境里就是那个常量: 不替换会让实例化出来的成员体留下
        // 裸 `N`, sema 报 `undeclared identifier: N` (g++ 同一位置正常)。
        // 标量**类型**实参 (如 `MangleType::Int`) 仍不替换 —— 它是类型不是值。
        CppExpr::Ident(name) => {
            if let Some(idx) = params.iter().position(|p| p == name) {
                match args.get(idx) {
                    Some(MangleType::Class(n)) => return CppExpr::Ident(n.clone()),
                    Some(MangleType::NonType(v)) => return CppExpr::IntLit(*v),
                    _ => {}
                }
            }
            e.clone()
        },
        // 字面量 / 模板名等无需替换
        other => other.clone(),
    }
}

/// 递归替换语句内的模板形参类型
pub fn substitute_stmt(s: &CppStmt, params: &[String], args: &[MangleType]) -> CppStmt {
    match s {
        CppStmt::Expr(e) => CppStmt::Expr(substitute_expr(e, params, args)),
        CppStmt::Return(e) => CppStmt::Return(e.as_ref().map(|x| substitute_expr(x, params, args))),
        CppStmt::If { cond, then, els } => CppStmt::If {
            cond: substitute_expr(cond, params, args),
            then: Box::new(substitute_body(then, params, args)),
            els: els
                .as_ref()
                .map(|x| Box::new(substitute_body(x, params, args))),
        },
        CppStmt::IfConstexpr { cond, then, els } => CppStmt::IfConstexpr {
            cond: substitute_expr(cond, params, args),
            then: Box::new(substitute_body(then, params, args)),
            els: els
                .as_ref()
                .map(|x| Box::new(substitute_body(x, params, args))),
        },
        CppStmt::While { cond, body } => CppStmt::While {
            cond: substitute_expr(cond, params, args),
            body: Box::new(substitute_body(body, params, args)),
        },
        CppStmt::For {
            init,
            cond,
            step,
            body,
        } => CppStmt::For {
            init: init
                .as_ref()
                .map(|x| Box::new(substitute_stmt(x, params, args))),
            cond: cond.as_ref().map(|x| substitute_expr(x, params, args)),
            step: step
                .as_ref()
                .map(|x| Box::new(substitute_stmt(x, params, args))),
            body: Box::new(substitute_body(body, params, args)),
        },
        CppStmt::Block(b) => CppStmt::Block(substitute_body(b, params, args)),
        CppStmt::ThisMember(n, inner) => {
            CppStmt::ThisMember(n.clone(), Box::new(substitute_stmt(inner, params, args)))
        },
        CppStmt::LocalVar {
            ty,
            name,
            init,
            is_volatile,
        } => {
            // M53: 局部变量**遮蔽**同名模板形参时整条不再替换 (`int N = 1;` 的
            // 初始化式按 C++ 作用域规则已经指向新变量)。
            if params.iter().any(|p| p == name) {
                return CppStmt::LocalVar {
                    ty: ty.clone(),
                    name: name.clone(),
                    init: init.clone(),
                    is_volatile: *is_volatile,
                };
            }
            CppStmt::LocalVar {
                ty: substitute_type(ty, params, args),
                name: name.clone(),
                init: init.as_ref().map(|x| substitute_expr(x, params, args)),
                is_volatile: *is_volatile,
            }
        },
        CppStmt::StaticLocal {
            ty,
            name,
            init,
            is_volatile,
        } => CppStmt::StaticLocal {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            init: init.as_ref().map(|x| substitute_expr(x, params, args)),
            is_volatile: *is_volatile,
        },
        CppStmt::LocalVarList { spec, declarators } => {
            // 共享的 decl-specifier 替换一次; 每个 declarator 只替换自己的初始化式
            // (`*` / `&` / `[N]` 层级不携带模板名, 原样保留)。
            // M53: 某个 declarator 的**名字遮蔽**了同名模板形参时, 它的初始化式
            // 按 C++ 作用域规则已经指向新变量, 那条不再替换。
            CppStmt::LocalVarList {
                spec: DeclSpec {
                    ty: substitute_type(&spec.ty, params, args),
                    is_static: spec.is_static,
                    is_volatile: spec.is_volatile,
                },
                declarators: declarators
                    .iter()
                    .map(|d| InitDeclarator {
                        layers: d.layers.clone(),
                        array_dims: d.array_dims.clone(),
                        name: d.name.clone(),
                        init: if params.iter().any(|p| p == &d.name) {
                            d.init.clone()
                        } else {
                            d.init.as_ref().map(|x| substitute_expr(x, params, args))
                        },
                    })
                    .collect(),
            }
        },
        CppStmt::Try { body, catches } => CppStmt::Try {
            body: Box::new(substitute_body(body, params, args)),
            catches: catches
                .iter()
                .map(|c| crate::cpp::parser::CatchClause {
                    ty: c.ty.as_ref().map(|t| substitute_type(t, params, args)),
                    name: c.name.clone(),
                    body: substitute_body(&c.body, params, args),
                })
                .collect(),
        },
        CppStmt::Throw(e) => CppStmt::Throw(e.as_ref().map(|x| substitute_expr(x, params, args))),
        CppStmt::CoAwait(e) => CppStmt::CoAwait(substitute_expr(e, params, args)),
        CppStmt::CoYield(e) => CppStmt::CoYield(substitute_expr(e, params, args)),
        CppStmt::CoReturn(e) => {
            CppStmt::CoReturn(e.as_ref().map(|x| substitute_expr(x, params, args)))
        },
        CppStmt::Decl(d) => CppStmt::Decl(substitute_decl(d, params, args, None)),
        other => other.clone(),
    }
}

/// 递归替换语句体
pub fn substitute_body(b: &CppBody, params: &[String], args: &[MangleType]) -> CppBody {
    CppBody {
        stmts: b
            .stmts
            .iter()
            .map(|s| substitute_stmt(s, params, args))
            .collect(),
        is_decl: b.is_decl,
    }
}

/// M51: 类模板的**注入类名**候选集合.
///
/// C++ 里类模板 `std::vector` 的类体内, 名字 `vector` 本身就是个类型名
/// (injected-class-name), 等价于 `std::vector<T>`. 它**不是**模板形参,
/// 因此通用的形参替换覆盖不到. 实例化时必须把它也绑到实例类名,
/// 否则 `vector(const vector& other)` 会留下裸的 `Class("vector")`,
/// sema 报 "unknown class type: vector".
///
/// 返回全名 + (若带命名空间限定) 去限定符短名, 因为 parser 对
/// `std::vector` 与 `vector` 两种写法可能产出不同的名字形态.
fn injected_class_names(name: &str) -> Vec<String> {
    let mut out = vec![name.to_string()];
    if let Some(short) = name.rsplit("::").next() {
        if short != name && !short.is_empty() {
            out.push(short.to_string());
        }
    }
    out
}

/// 递归替换声明内的模板形参类型
/// class_rename: 类模板实例化时新的类名 (成员函数/ctor 的 class 字段同步改写)
pub fn substitute_decl(
    d: &CppDecl,
    params: &[String],
    args: &[MangleType],
    class_rename: Option<&str>,
) -> CppDecl {
    /// M53: 把被函数形参**遮蔽**的模板形参从替换表里摘掉 (params/args 下标
    /// 对齐关系保持不变): `template<int N> struct S { int f(int N) { return N; } }`
    /// 体里的 `N` 是那个形参, 不是模板实参.
    fn shield_params(
        params: &[String],
        args: &[MangleType],
        shadowing: &[String],
    ) -> (Vec<String>, Vec<MangleType>) {
        let mut p = Vec::with_capacity(params.len());
        let mut a = Vec::with_capacity(params.len());
        for (i, name) in params.iter().enumerate() {
            if shadowing.iter().any(|s| s == name) {
                continue;
            }
            if let Some(x) = args.get(i) {
                p.push(name.clone());
                a.push(x.clone());
            }
        }
        (p, a)
    }

    /// 函数体替换表 = 原表去掉被形参遮蔽的条目
    fn body_subst(
        params: &[String],
        args: &[MangleType],
        fn_ps: &[(String, MangleType)],
    ) -> (Vec<String>, Vec<MangleType>) {
        let names: Vec<String> = fn_ps.iter().map(|(n, _)| n.clone()).collect();
        shield_params(params, args, &names)
    }

    match d {
        CppDecl::Class(c) => {
            let new_name = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| c.name.clone());
            // M51: 类模板实例化时把**注入类名** (injected-class-name) 也绑到
            // 实例名: `vector(const vector& other)` 里的 `vector` 不是模板形参,
            // 不绑就漏下 `Class("vector")`, sema 报 "unknown class type: vector".
            // 追加在形参表末尾 → 原有 T/U → 实参 的下标对齐关系不变.
            let (mem_params, mem_args) = match class_rename {
                Some(new) => {
                    let mut p = params.to_vec();
                    let mut a = args.to_vec();
                    for n in injected_class_names(&c.name) {
                        if p.contains(&n) {
                            continue;
                        }
                        p.push(n);
                        a.push(MangleType::Class(new.to_string()));
                    }
                    (p, a)
                },
                None => (params.to_vec(), args.to_vec()),
            };
            CppDecl::Class(ClassDecl {
                name: new_name,
                is_struct: c.is_struct,
                is_union: c.is_union,
                bases: c.bases.clone(),
                members: c
                    .members
                    .iter()
                    .map(|m| substitute_decl(m, &mem_params, &mem_args, class_rename))
                    .collect(),
                member_access: c.member_access.clone(),
            })
        },
        CppDecl::FreeFn {
            ret,
            name,
            params: ps,
            body,
            is_noexcept,
            trailing_ret_type,
            ..
        } => CppDecl::FreeFn {
            ret: substitute_type(ret, params, args),
            name: name.clone(),
            params: ps
                .iter()
                .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                .collect(),
            body: {
                let (bp, ba) = body_subst(params, args, ps);
                substitute_body(body, &bp, &ba)
            },
            is_noexcept: *is_noexcept,
            trailing_ret_type: trailing_ret_type.clone(),
        },
        CppDecl::MemberFn {
            class,
            ret,
            name,
            params: ps,
            body,
            is_virtual,
            is_override,
            is_const,
            is_final,
            is_noexcept,
            is_volatile,
            is_friend,
            ..
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| class.clone());
            CppDecl::MemberFn {
                class: new_class,
                ret: substitute_type(ret, params, args),
                name: name.clone(),
                params: ps
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                    .collect(),
                body: {
                    let (bp, ba) = body_subst(params, args, ps);
                    substitute_body(body, &bp, &ba)
                },
                is_virtual: *is_virtual,
                is_override: *is_override,
                is_const: *is_const,
                is_final: *is_final,
                is_noexcept: *is_noexcept,
                is_volatile: *is_volatile,
                is_friend: *is_friend,
            }
        },
        CppDecl::Ctor {
            class,
            params: ps,
            inits,
            body,
            is_virtual,
            is_explicit,
            is_noexcept,
            ..
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| class.clone());
            CppDecl::Ctor {
                class: new_class,
                params: ps
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                    .collect(),
                inits: {
                    let (bp, ba) = body_subst(params, args, ps);
                    inits
                        .iter()
                        .map(|(n, e)| (n.clone(), substitute_expr(e, &bp, &ba)))
                        .collect()
                },
                body: {
                    let (bp, ba) = body_subst(params, args, ps);
                    substitute_body(body, &bp, &ba)
                },
                is_virtual: *is_virtual,
                is_explicit: *is_explicit,
                is_noexcept: *is_noexcept,
            }
        },
        CppDecl::Dtor {
            class,
            body,
            is_virtual,
            is_noexcept,
            ..
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| class.clone());
            CppDecl::Dtor {
                class: new_class,
                body: substitute_body(body, params, args),
                is_virtual: *is_virtual,
                is_noexcept: *is_noexcept,
            }
        },
        CppDecl::MemberVar {
            ty,
            name,
            is_mutable,
            is_volatile,
            ..
        } => CppDecl::MemberVar {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            is_mutable: *is_mutable,
            is_volatile: *is_volatile,
        },
        CppDecl::OperatorFn {
            class,
            op,
            params: ps,
            body,
            ret,
            is_unary,
        } => {
            let new_class = class_rename
                .map(|n| n.to_string())
                .or_else(|| class.clone());
            CppDecl::OperatorFn {
                class: new_class,
                op: op.clone(),
                params: ps
                    .iter()
                    .map(|(n, t)| (n.clone(), substitute_type(t, params, args)))
                    .collect(),
                body: substitute_body(body, params, args),
                ret: substitute_type(ret, params, args),
                is_unary: *is_unary,
            }
        },
        CppDecl::Namespace { name, body } => CppDecl::Namespace {
            name: name.clone(),
            body: body
                .iter()
                .map(|x| substitute_decl(x, params, args, class_rename))
                .collect(),
        },
        // 嵌套模板声明: 保留 (实例化时按需再次实例化)
        CppDecl::Template {
            params: tp,
            decl: inner,
            requires,
            is_export,
        } => CppDecl::Template {
            params: tp.clone(),
            decl: Box::new(substitute_decl(inner, params, args, class_rename)),
            requires: requires.clone(),
            is_export: *is_export,
        },
        // M30: typedef 目标类型形参替换 (`typedef T type;` 在实例
        // 类中变为 `typedef <Arg> type;`, 供 TplMember 引用展开)
        CppDecl::Typedef { target, name } => CppDecl::Typedef {
            target: substitute_type(target, params, args),
            name: name.clone(),
        },
        other => other.clone(),
    }
}

/// 实例化入口: 替换模板体内所有类型形参
/// class_rename: 类模板实例化时的新类名
pub fn instantiate_decl(
    decl: &CppDecl,
    params: &[String],
    args: &[MangleType],
    _orig_name: Option<&str>,
    _orig_args: &[MangleType],
) -> CppDecl {
    let rename = match decl {
        CppDecl::Class(_) => Some(instantiated_class_name(_orig_name.unwrap_or(""), _orig_args)),
        _ => None,
    };
    substitute_decl(decl, params, args, rename.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, args: &[MangleType]) -> MangleType {
        MangleType::Template {
            name: name.to_string(),
            args: args.to_vec(),
        }
    }

    #[test]
    fn substitute_functional_cast_preserves_value() {
        // `T(9)` with T=int → functional cast, 必须保留实参值 (Cast(Int, 9)), 不能归零.
        let e = CppExpr::Call(Box::new(CppExpr::Ident("T".to_string())), vec![CppExpr::IntLit(9)]);
        match substitute_expr(&e, &["T".to_string()], &[MangleType::Int]) {
            CppExpr::Cast(MangleType::Int, inner) => {
                assert!(matches!(*inner, CppExpr::IntLit(9)));
            },
            other => panic!("expected Cast(Int, 9), got {:?}", other),
        }
        // `T()` 空实参 → 值初始化 0.
        let e0 = CppExpr::Call(Box::new(CppExpr::Ident("T".to_string())), vec![]);
        assert!(matches!(
            substitute_expr(&e0, &["T".to_string()], &[MangleType::Int]),
            CppExpr::IntLit(0)
        ));
    }

    #[test]
    fn substitute_non_class_param_not_zeroed() {
        // 裸 Ident 参数名在绑定为非类类型 (如非类型模板参数) 时不得被替换成 0.
        let e = CppExpr::Ident("N".to_string());
        let r = substitute_expr(&e, &["N".to_string()], &[MangleType::Int]);
        assert!(matches!(&r, CppExpr::Ident(name) if name == "N"), "非类参数名被误替换: {:?}", r);
        // 绑定为类类型时, 裸 Ident 替换为实际类名.
        let rc = substitute_expr(
            &CppExpr::Ident("T".to_string()),
            &["T".to_string()],
            &[MangleType::Class("Foo".to_string())],
        );
        assert!(matches!(&rc, CppExpr::Ident(name) if name == "Foo"));
    }

    #[test]
    fn canonical_key_uniqueness() {
        // Section 7.1: template canonical key - type_key 必须为不同类型生成
        // 不同 key (缓存命中依据), 且对嵌套/模板类型保持结构.
        use MangleType as M;
        // 基础类型互异
        let mut keys = std::collections::HashSet::new();
        for ty in [
            M::Void,
            M::Int,
            M::Uint,
            M::Long,
            M::LongLong,
            M::Float,
            M::Double,
            M::Bool,
            M::Char,
        ] {
            assert!(keys.insert(type_key(&ty)), "dup key for {ty:?}");
        }
        // 指针 vs 引用不同
        let p = M::Pointer(Box::new(M::Int));
        let r = M::Reference(Box::new(M::Int));
        assert_ne!(type_key(&p), type_key(&r));
        // 嵌套模板 vector<vector<int>> 保持递归结构
        let inner = t("vector", &[M::Int]);
        let outer = t("vector", std::slice::from_ref(&inner));
        let flat = t("vector", &[M::Int]);
        assert_ne!(type_key(&outer), type_key(&flat));
        assert_eq!(type_key(&inner), "t_vector_int");
        // 非类型实参 key
        let n0 = M::NonType(0);
        let n1 = M::NonType(1);
        assert_ne!(type_key(&n0), type_key(&n1));
        assert_eq!(type_key(&n1), "n1");
    }

    #[test]
    fn substitute_type_basic() {
        let params = ["T".to_string(), "U".to_string()];
        let args = [MangleType::Int, MangleType::Class("MyClass".to_string())];
        assert_eq!(
            substitute_type(&MangleType::Class("T".to_string()), &params, &args),
            MangleType::Int
        );
        assert_eq!(
            substitute_type(&MangleType::Class("U".to_string()), &params, &args),
            MangleType::Class("MyClass".to_string())
        );
        // 非形参保留
        assert_eq!(
            substitute_type(&MangleType::Class("X".to_string()), &params, &args),
            MangleType::Class("X".to_string())
        );
        // 指针递归
        assert_eq!(
            substitute_type(
                &MangleType::Pointer(Box::new(MangleType::Class("T".to_string()))),
                &params,
                &args
            ),
            MangleType::Pointer(Box::new(MangleType::Int))
        );
        // 模板嵌套: vector<T>
        assert_eq!(
            substitute_type(&t("vector", &[MangleType::Class("T".to_string())]), &params, &args),
            t("vector", &[MangleType::Int])
        );
    }

    #[test]
    fn instantiate_class_renames_members() {
        let mut reg = TemplateRegistry::default();
        // template <class T> struct Box { T payload; Box(T v) : payload(v) {} };
        let inner = CppDecl::Class(ClassDecl {
            name: "Box".to_string(),
            is_struct: true,
            is_union: false,
            bases: vec![],
            member_access: vec![],
            members: vec![
                CppDecl::MemberVar {
                    ty: MangleType::Class("T".to_string()),
                    name: "payload".to_string(),
                    is_mutable: false,
                    is_volatile: false,
                },
                CppDecl::Ctor {
                    class: "Box".to_string(),
                    params: vec![("v".to_string(), MangleType::Class("T".to_string()))],
                    inits: vec![("payload".to_string(), CppExpr::Ident("v".to_string()))],
                    body: CppBody {
                        stmts: vec![],
                        is_decl: false,
                    },
                    is_virtual: false,
                    is_explicit: false,
                    is_noexcept: false,
                },
            ],
        });
        reg.register(
            &TemplateArgs {
                params: vec![crate::cpp::parser::TemplateParam {
                    template_params: None,
                    name: "T".to_string(),
                    default: None,
                    non_type: None,
                    is_pack: false,
                }],
            },
            &None,
            &inner,
        );
        let inst = reg
            .instantiate_class("Box", &[MangleType::Int])
            .expect("instantiate");
        match inst {
            CppDecl::Class(c) => {
                assert_eq!(c.name, "__dcc_template_Box_int");
                assert_eq!(c.members.len(), 2);
                match &c.members[0] {
                    CppDecl::MemberVar { ty, name, .. } => {
                        assert_eq!(*ty, MangleType::Int);
                        assert_eq!(name, "payload");
                    },
                    _ => panic!("member 0 not MemberVar"),
                }
                match &c.members[1] {
                    CppDecl::Ctor { class, params, .. } => {
                        assert_eq!(class, "__dcc_template_Box_int");
                        assert_eq!(params[0].1, MangleType::Int);
                    },
                    _ => panic!("member 1 not Ctor"),
                }
            },
            _ => panic!("not class"),
        }
        // 缓存: 再次实例化返回相同结果
        let inst2 = reg
            .instantiate_class("Box", &[MangleType::Int])
            .expect("cached");
        match inst2 {
            CppDecl::Class(c) => assert_eq!(c.name, "__dcc_template_Box_int"),
            _ => panic!("not class"),
        }
    }

    #[test]
    fn deduce_args_from_call() {
        let mut reg = TemplateRegistry::default();
        // template <typename T, typename U> T sfinae_first(T a, U b) { return a; }
        let inner = CppDecl::FreeFn {
            ret: MangleType::Class("T".to_string()),
            name: "sfinae_first".to_string(),
            params: vec![
                ("a".to_string(), MangleType::Class("T".to_string())),
                ("b".to_string(), MangleType::Class("U".to_string())),
            ],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::Ident("a".to_string())))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        };
        let targs = TemplateArgs {
            params: vec![
                crate::cpp::parser::TemplateParam {
                    template_params: None,
                    name: "T".to_string(),
                    default: None,
                    non_type: None,
                    is_pack: false,
                },
                crate::cpp::parser::TemplateParam {
                    template_params: None,
                    name: "U".to_string(),
                    default: None,
                    non_type: None,
                    is_pack: false,
                },
            ],
        };
        reg.register(&targs, &None, &inner);
        let td = reg.find("sfinae_first").unwrap();
        let args = reg
            .deduce_args(td, &[MangleType::Long, MangleType::Double])
            .expect("deduce");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], MangleType::Long);
        assert_eq!(args[1], MangleType::Double);
    }

    #[test]
    fn deduce_args_default_only() {
        let mut reg = TemplateRegistry::default();
        // template <typename T = int> int sfinae_default_arg() { return 42; }
        let inner = CppDecl::FreeFn {
            ret: MangleType::Int,
            name: "sfinae_default_arg".to_string(),
            params: vec![],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::IntLit(42)))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        };
        let targs = TemplateArgs {
            params: vec![crate::cpp::parser::TemplateParam {
                template_params: None,
                name: "T".to_string(),
                default: Some(MangleType::Int),
                non_type: None,
                is_pack: false,
            }],
        };
        reg.register(&targs, &None, &inner);
        let td = reg.find("sfinae_default_arg").unwrap();
        let args = reg.deduce_args(td, &[]).expect("default args");
        assert_eq!(args, vec![MangleType::Int]);
    }

    #[test]
    fn requires_constraint_filters() {
        let mut reg = TemplateRegistry::default();
        // template <typename T> requires sizeof(T) <= 4 T pick(T x) { return x; }
        let inner = CppDecl::FreeFn {
            ret: MangleType::Class("T".to_string()),
            name: "pick".to_string(),
            params: vec![("x".to_string(), MangleType::Class("T".to_string()))],
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::Ident("x".to_string())))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        };
        let targs = TemplateArgs {
            params: vec![crate::cpp::parser::TemplateParam {
                template_params: None,
                name: "T".to_string(),
                default: None,
                non_type: None,
                is_pack: false,
            }],
        };
        reg.register(&targs, &Some("sizeof(T) <= 4".to_string()), &inner);
        // int 满足 sizeof(int) <= 4
        assert!(reg.instantiate_fn("pick", &[MangleType::Int]).is_some());
        // long long 不满足 → SFINAE 跳过
        assert!(reg
            .instantiate_fn("pick", &[MangleType::LongLong])
            .is_none());
    }

    #[test]
    fn partial_spec_pick() {
        let mut reg = TemplateRegistry::default();
        reg.partials.push(PartialSpecEntry {
            name: "Foo".to_string(),
            param_names: vec!["T".to_string()],
            pattern_text: "T*".to_string(),
            decl: CppDecl::Class(ClassDecl {
                name: "Foo".to_string(),
                is_struct: true,
                is_union: false,
                bases: vec![],
                member_access: vec![],
                members: vec![],
            }),
        });
        // int* 匹配 T*
        let picked = reg.pick_partial("Foo", &[MangleType::Pointer(Box::new(MangleType::Int))]);
        assert!(picked.is_some());
        // int 不匹配
        assert!(reg.pick_partial("Foo", &[MangleType::Int]).is_none());
    }

    /// 指针偏特化的形参必须绑定到**被指对象**, 而不是整个指针类型.
    /// 回归: `iterator_traits<T*>` 的 `typedef T value_type` 对 `int*`
    /// 曾错误地得到 `int*` (尺寸 8), 半个 STL 迭代器 traits 因此失效.
    #[test]
    fn partial_spec_pointer_binds_pointee_not_pointer() {
        let mut reg = TemplateRegistry::default();
        reg.partials.push(PartialSpecEntry {
            name: "traits".to_string(),
            param_names: vec!["T".to_string()],
            pattern_text: "T*".to_string(),
            decl: CppDecl::Class(ClassDecl {
                name: "traits".to_string(),
                is_struct: true,
                is_union: false,
                bases: vec![],
                member_access: vec![],
                members: vec![CppDecl::Typedef {
                    target: MangleType::Class("T".to_string()),
                    name: "value_type".to_string(),
                }],
            }),
        });
        let p = |t: MangleType| MangleType::Pointer(Box::new(t));
        for (arg, expect) in [
            (p(MangleType::Int), MangleType::Int),
            (p(MangleType::Char), MangleType::Char),
            (p(p(MangleType::Int)), p(MangleType::Int)),
        ] {
            let (_, names, bound) = reg.pick_partial("traits", &[arg]).expect("T* 应匹配");
            assert_eq!(names, vec!["T".to_string()]);
            assert_eq!(bound, vec![expect], "形参 T 的绑定应是被指对象本身");
        }
        // 非指针实参不匹配 `T*`
        assert!(reg.pick_partial("traits", &[MangleType::Int]).is_none());
    }
}

// ===== G14: Route A - Dependent Name Resolution Tests =====

#[cfg(test)]
mod g14_tests {
    use super::*;
    use crate::cpp::parser::{CppExpr, TemplateParam};

    #[test]
    fn test_is_template_parameter() {
        // Verify basic parameter detection
        let params = vec![
            TemplateParam {
                template_params: None,
                name: "T".to_string(),
                default: None,
                non_type: None,
                is_pack: false,
            },
            TemplateParam {
                template_params: None,
                name: "U".to_string(),
                default: None,
                non_type: None,
                is_pack: false,
            },
        ];

        assert!(crate::cpp::parser::is_template_parameter("T", &params));
        assert!(crate::cpp::parser::is_template_parameter("U", &params));
        assert!(!crate::cpp::parser::is_template_parameter("X", &params));
        assert!(!crate::cpp::parser::is_template_parameter("int", &params));
    }

    #[test]
    fn test_dependent_expression_detection() {
        let params = vec![TemplateParam {
            template_params: None,
            name: "T".to_string(),
            default: None,
            non_type: None,
            is_pack: false,
        }];

        // Ident 'T' should be dependent
        assert!(crate::cpp::parser::is_dependent_expression(
            &CppExpr::Ident("T".to_string()),
            &params
        ));

        // Ident 'x' should NOT be dependent
        assert!(!crate::cpp::parser::is_dependent_expression(
            &CppExpr::Ident("x".to_string()),
            &params
        ));

        // Member access with dependent object
        let obj = Box::new(CppExpr::Ident("T".to_string()));
        assert!(crate::cpp::parser::is_dependent_expression(
            &CppExpr::Member(obj, "member".to_string()),
            &params
        ));
    }

    #[test]
    fn test_substitute_and_resolve_basic() {
        let mut reg = TemplateRegistry::default();

        // Create a simple template class
        let inner = CppDecl::Class(ClassDecl {
            name: "Box".to_string(),
            is_struct: true,
            is_union: false,
            bases: vec![],
            member_access: vec![],
            members: vec![CppDecl::MemberVar {
                ty: MangleType::Class("T".to_string()),
                name: "payload".to_string(),
                is_mutable: false,
                is_volatile: false,
            }],
        });

        reg.register(
            &TemplateArgs {
                params: vec![TemplateParam {
                    template_params: None,
                    name: "T".to_string(),
                    default: None,
                    non_type: None,
                    is_pack: false,
                }],
            },
            &None,
            &inner,
        );

        // Instantiate with int
        let result = reg.instantiate_class("Box", &[MangleType::Int]);
        assert!(result.is_some());

        let inst = result.unwrap();
        match inst {
            CppDecl::Class(c) => {
                // Should have renamed the class
                assert_eq!(c.name, "__dcc_template_Box_int");

                // The member type should be substituted
                if let CppDecl::MemberVar { ty, .. } = &c.members[0] {
                    assert_eq!(*ty, MangleType::Int);
                } else {
                    panic!("Expected MemberVar");
                }
            },
            _ => panic!("Expected Class"),
        }
    }

    #[test]
    fn test_dependent_nested_type_pattern() {
        // Test pattern similar to std::reverse_iterator::difference_type
        // template<typename _Iterator>
        // class reverse_iterator { typedef typename _Iterator::difference_type diff; };

        let mut reg = TemplateRegistry::default();

        // Simulate nested type pattern: _Iterator::difference_type
        let inner = CppDecl::Class(ClassDecl {
            name: "reverse_iterator".to_string(),
            is_struct: true,
            is_union: false,
            bases: vec![],
            member_access: vec![],
            members: vec![CppDecl::Typedef {
                target: MangleType::TplMember {
                    name: "_Iterator".to_string(),
                    args: vec![],
                    member: "difference_type".to_string(),
                },
                name: "difference_type".to_string(),
            }],
        });

        reg.register(
            &TemplateArgs {
                params: vec![TemplateParam {
                    template_params: None,
                    name: "_Iterator".to_string(),
                    default: None,
                    non_type: None,
                    is_pack: false,
                }],
            },
            &None,
            &inner,
        );

        // Instantiate with vector<int>::iterator-like type
        let iterator_type = MangleType::Template {
            name: "vector".to_string(),
            args: vec![MangleType::Int],
        };

        let result = reg.instantiate_class("reverse_iterator", std::slice::from_ref(&iterator_type));
        assert!(result.is_some());

        let inst = result.unwrap();
        match inst {
            CppDecl::Class(c) => {
                // Check that typedef was instantiated
                for member in &c.members {
                    if let CppDecl::Typedef { target, .. } = member {
                        // The target should still be TplMember pattern (text substitution doesn't fully resolve)
                        match target {
                            MangleType::TplMember {
                                name: _name,
                                member,
                                ..
                            } => {
                                // Name stays as template param name (_Iterator), but type args would be substituted
                                // For this test, we just verify it's still a TplMember after instantiation
                                assert_eq!(member, "difference_type");
                            },
                            _ => panic!("Expected TplMember after instantiation, got {:?}", target),
                        }
                    }
                }
            },
            _ => panic!("Expected Class"),
        }
    }

    #[test]
    fn test_instantiation_context_binding() {
        use crate::cpp::sema::InstantiationContext;

        let mut ctx = InstantiationContext::new();

        // Bind template parameter T to int (base type, no members)
        ctx.bind("T".to_string(), MangleType::Int);
        // Bind Container to vector<double> (has nested types)
        ctx.bind(
            "Container".to_string(),
            MangleType::Template {
                name: "vector".to_string(),
                args: vec![MangleType::Double],
            },
        );

        // Lookup on base type should return None (int has no members)
        assert!(ctx.lookup_dependent_member("T", "anything").is_none());

        // Lookup on template type should return Some
        assert!(ctx
            .lookup_dependent_member("Container", "iterator")
            .is_some());

        // Non-existent parameter should return None
        assert!(ctx.lookup_dependent_member("X", "anything").is_none());
    }
}

// ===== G11: 模板模板参数实例化逻辑 =====

/// G11: 替换类型中的模板模板参数引用
fn substitute_template_refs_in_type(
    ty: &MangleType,
    bindings: &std::collections::HashMap<String, MangleType>,
) -> MangleType {
    match ty {
        // 检查是否是模板模板参数引用
        MangleType::Class(name) => {
            if let Some(bound) = bindings.get(name) {
                return bound.clone();
            }
            ty.clone()
        },

        MangleType::Pointer(inner) => {
            MangleType::Pointer(Box::new(substitute_template_refs_in_type(inner, bindings)))
        },
        MangleType::Reference(inner) => {
            MangleType::Reference(Box::new(substitute_template_refs_in_type(inner, bindings)))
        },
        MangleType::Array(inner, size) => {
            MangleType::Array(Box::new(substitute_template_refs_in_type(inner, bindings)), *size)
        },

        MangleType::Template { name, args } => {
            let new_args: Vec<MangleType> = args
                .iter()
                .map(|a| substitute_template_refs_in_type(a, bindings))
                .collect();
            MangleType::Template {
                name: name.clone(),
                args: new_args,
            }
        },

        MangleType::TplMember { name, args, member } => {
            let new_args: Vec<MangleType> = args
                .iter()
                .map(|a| substitute_template_refs_in_type(a, bindings))
                .collect();
            MangleType::TplMember {
                name: name.clone(),
                args: new_args,
                member: member.clone(),
            }
        },

        _ => ty.clone(),
    }
}

/// G11: 替换声明中的模板模板参数引用（简化版本）
fn substitute_template_refs_in_decl(
    decl: &CppDecl,
    bindings: &std::collections::HashMap<String, MangleType>,
) -> CppDecl {
    // 先执行普通的类型参数替换
    let substituted = substitute_decl(decl, &[], &[], None);

    // 然后对关键成员进行模板模板参数替换
    match &substituted {
        CppDecl::Class(c) => {
            // 递归替换类成员中的模板模板参数
            let new_members: Vec<CppDecl> = c
                .members
                .iter()
                .map(|m| match m {
                    CppDecl::MemberVar {
                        ty,
                        name,
                        is_mutable,
                        is_volatile,
                        ..
                    } => CppDecl::MemberVar {
                        ty: substitute_template_refs_in_type(ty, bindings),
                        name: name.clone(),
                        is_mutable: *is_mutable,
                        is_volatile: *is_volatile,
                    },
                    _ => m.clone(),
                })
                .collect();

            CppDecl::Class(ClassDecl {
                name: c.name.clone(),
                is_struct: c.is_struct,
                is_union: c.is_union,
                bases: c.bases.clone(),
                member_access: c.member_access.clone(),
                members: new_members,
            })
        },
        _ => substituted,
    }
}

/// G11: 增强版的 instantiate_decl，支持模板模板参数替换
pub fn instantiate_decl_with_template_params(
    decl: &CppDecl,
    params: &[String],
    args: &[MangleType],
    class_rename: Option<&str>,
    primary_params: &[crate::cpp::parser::TemplateParam],
) -> CppDecl {
    // Step 1: 构建普通参数和模板模板参数的绑定
    let mut bindings = std::collections::HashMap::new();
    for (i, param_name) in params.iter().enumerate() {
        if let Some(arg) = args.get(i) {
            bindings.insert(param_name.clone(), arg.clone());
        }
    }

    // Step 2: 识别模板模板参数并添加到绑定中
    for (i, param_name) in params.iter().enumerate() {
        if let Some(primary_param) = primary_params.get(i) {
            if let Some(_arity) = primary_param.template_params {
                if let Some(arg) = args.get(i) {
                    bindings.insert(param_name.clone(), arg.clone());
                }
            }
        }
    }

    // Step 3: 先执行普通替换，再执行模板模板参数的特殊替换
    let substituted = substitute_decl(decl, params, args, class_rename);
    substitute_template_refs_in_decl(&substituted, &bindings)
}

// ===== END OF G11 CODE =====

// ===== M51: concept / requires / SFINAE 候选集语义回归测试 =====

#[cfg(test)]
mod m51_tests {
    use super::*;

    /// 造一个自由函数模板体 (只用于注册候选, 不要求可执行)
    fn fn_tpl(name: &str, params: Vec<(String, MangleType)>, ret: MangleType) -> CppDecl {
        CppDecl::FreeFn {
            ret,
            name: name.to_string(),
            params,
            body: CppBody {
                stmts: vec![CppStmt::Return(Some(CppExpr::IntLit(0)))],
                is_decl: false,
            },
            is_noexcept: false,
            trailing_ret_type: None,
        }
    }

    /// 造 `template <typename A, typename B = ..., ...>` 形参表
    fn tparams(names: &[&str], defaults: &[Option<MangleType>]) -> TemplateArgs {
        TemplateArgs {
            params: names
                .iter()
                .enumerate()
                .map(|(i, n)| crate::cpp::parser::TemplateParam {
                    template_params: None,
                    name: (*n).to_string(),
                    default: defaults.get(i).cloned().flatten(),
                    non_type: None,
                    is_pack: false,
                })
                .collect(),
        }
    }

    #[test]
    fn concept_ref_expansion_slices_at_angle_bracket() {
        // `Small<T>`: 名字占 5 个字符. 旧实现按 `start + 1..end` 取实参,
        // 会切出 `mall<T`, 概念形参被绑成垃圾, 展开结果整个失真.
        let mut reg = TemplateRegistry::default();
        reg.register_concept("Small", &["T".to_string()], "sizeof(T) <= 4");
        let expanded = reg.expand_concept_refs("Small<T>");
        assert!(!expanded.contains("mall"), "实参切片错位: {expanded}");
        assert!(expanded.contains("sizeof"), "概念体未被代入: {expanded}");
        let mut ctx = SfinaeCtx::new();
        ctx.bind("T".to_string(), "int".to_string());
        assert_eq!(reg.evaluate_constraint("Small<T>", &ctx), Some(true));
        ctx.bind("T".to_string(), "long long".to_string());
        assert_eq!(reg.evaluate_constraint("Small<T>", &ctx), Some(false));
    }

    #[test]
    fn multi_param_concept_binds_all_params() {
        // 形参不只一个、且不叫 T 的概念: 逐个按名字绑定, 不与外层形参串味
        let mut reg = TemplateRegistry::default();
        reg.register_concept(
            "SameAs",
            &["A".to_string(), "B".to_string()],
            "is_same<A, B>::value",
        );
        let mut ctx = SfinaeCtx::new();
        ctx.bind("T".to_string(), "int".to_string());
        assert_eq!(reg.evaluate_constraint("SameAs<T, int>", &ctx), Some(true));
        ctx.bind("T".to_string(), "long".to_string());
        assert_eq!(reg.evaluate_constraint("SameAs<T, int>", &ctx), Some(false));
    }

    #[test]
    fn overloaded_candidates_keep_their_index() {
        // `requires` 版 + 无约束版同名重载: 推导必须返回**候选下标**,
        // 否则两个不同函数会共用一个实例化缓存键 / 一个 C 符号.
        let mut reg = TemplateRegistry::default();
        reg.register_concept("Small", &["T".to_string()], "sizeof(T) <= 4");
        let one = tparams(&["T"], &[]);
        reg.register(
            &one,
            &Some("Small<T>".to_string()),
            &fn_tpl("tag", vec![("x".to_string(), MangleType::Class("T".to_string()))], MangleType::Int),
        );
        reg.register(
            &one,
            &None,
            &fn_tpl("tag", vec![("y".to_string(), MangleType::Class("T".to_string()))], MangleType::Int),
        );
        assert_eq!(reg.candidates_named("tag").len(), 2);
        // int 满足 Small → 候选 0; long long 不满足 → 静默换下一个 → 候选 1
        let hit = reg.deduce_for_call("tag", &[MangleType::Int]).expect("int");
        assert_eq!(hit.0, 0);
        let hit2 = reg
            .deduce_for_call("tag", &[MangleType::LongLong])
            .expect("long long");
        assert_eq!(hit2.0, 1);
        // 名字+实参相同、候选不同 → 缓存键必须不同
        assert_ne!(
            TemplateRegistry::fn_inst_key(0, "tag", &[MangleType::Int]),
            TemplateRegistry::fn_inst_key(1, "tag", &[MangleType::Int])
        );
    }

    #[test]
    fn anonymous_default_param_is_filled_by_resolution() {
        // `template <typename T, typename = void> int sfinae_anon(T)`
        let mut reg = TemplateRegistry::default();
        let args = tparams(&["T", ""], &[None, Some(MangleType::Void)]);
        reg.register(
            &args,
            &None,
            &fn_tpl("sfinae_anon", vec![("x".to_string(), MangleType::Class("T".to_string()))], MangleType::Int),
        );
        let (idx, resolved) = reg
            .normalize_args("sfinae_anon", &[MangleType::Int])
            .expect("匿名默认形参应被填充");
        assert_eq!(idx, 0);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], MangleType::Int);
        assert_eq!(resolved[1], MangleType::Void);
    }

    #[test]
    fn no_match_diagnostic_is_gated_on_function_templates() {
        let mut reg = TemplateRegistry::default();
        let one = tparams(&["T"], &[]);
        reg.register(&one, &None, &fn_tpl("pick", vec![], MangleType::Int));
        assert!(reg.only_fn_templates_named("pick"));
        assert!(!reg.only_fn_templates_named("nowhere"));
        // 类模板同名时, 调用位置是临时量构造而不是函数调用 → 不得报"无匹配函数"
        reg.register(
            &one,
            &None,
            &CppDecl::Class(ClassDecl {
                name: "pick".to_string(),
                is_struct: true,
                is_union: false,
                bases: vec![],
                member_access: vec![],
                members: vec![],
            }),
        );
        assert!(!reg.only_fn_templates_named("pick"));
    }

    #[test]
    fn injected_class_names_covers_qualified_and_short() {
        assert_eq!(injected_class_names("Box"), vec!["Box".to_string()]);
        let names = injected_class_names("std::vector");
        assert_eq!(names, vec!["std::vector".to_string(), "vector".to_string()]);
    }
}
