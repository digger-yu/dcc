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
use crate::cpp::cpp_const_eval::eval_nt_expr;
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
            },
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
    /// 被特化的主模板名
    pub name: String,
    /// 指定的类型实参 [void] 或 [int, double] 等
    pub specific_args: Vec<MangleType>,
    /// 特化体的**完整**类声明: 基类列表 + 全部成员 + 成员访问级别.
    /// 早前只存 `CppBody`, 展开时仅取回 MemberVar/Typedef —— 基类与
    /// `enum { value = N }` / 成员函数这类静默丢失会直接算错 trait 取值.
    pub class: crate::cpp::parser::ClassDecl,
}

/// 普通 (非模板) 类里参与**编译期常量折叠**的信息.
///
/// [temp.arg.nontype] 规定非类型实参是一个常量表达式, 而 traits 惯用法把
/// 这个常量写在类体里 (`enum { value = N }` / `static const bool value = ...`),
/// 并且绝大多数实例的 `value` 是**继承**来的
/// (`struct is_pointer<T*> : true_type {}`). 模板实参折叠必须沿这条继承链取值;
/// 普通类不是模板实例, 不在 `inst_cache` 里, 因此单独建一张表.
#[derive(Debug, Clone, Default)]
pub struct PlainClassInfo {
    /// 类体内可折成整数的常量 (成员名 -> 值)
    consts: Vec<(String, i64)>,
    /// 基类 (类名 -> 模板实参; 非模板基类实参为空)
    bases: Vec<(String, Vec<MangleType>)>,
    /// 类体引入的**成员名** (typedef / 嵌套类 / enum 及其枚举量 / 变量 / 函数)。
    /// [temp.deduct]/8 的“依赖名是否存在”判据只看名字, 不需要代入后的类型。
    member_names: Vec<String>,
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
    /// 普通类的编译期常量与基类链 (键为**源码书写形态**的类名, 同时登记剥掉
    /// 命名空间前缀的末段名). 见 [`PlainClassInfo`].
    pub plain_classes: std::collections::HashMap<String, PlainClassInfo>,
    /// **typedef / using 别名** → 目标类型 (键名规则与 [`Self::plain_classes`] 相同)。
    ///
    /// traits 惯用法几乎都靠别名缩短书写:
    /// ```cpp
    /// typedef integral_constant<bool, true> true_type;
    /// template <typename T> struct is_pointer : false_type {};
    /// ```
    /// `true_type` 不是类名而是别名, 因此 `true_type::value` 与基类子句里的
    /// `: false_type` 都不在 `plain_classes` 里 —— 必须有一张别名表把名字还原成
    /// 目标类型后再走同一条常量/成员查找, 否则继承链在这里断掉。
    pub type_aliases: std::collections::HashMap<String, MangleType>,
}

/// 取限定名的**末段** (`std::vector` -> `vector`); 无 `::` 时原样返回。
///
/// 命名空间里的模板在 AST 里只带短名, 而使用点可能写限定名 (或反过来);
/// 名字归一 ([`TemplateRegistry::canonical_name`]) 就是按这条短名对齐两种书写。
fn short_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
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
        // [temp.dep.constexpr]: 未折叠的依赖非类型子式。操作数名进入 key,
        // 保证 `!is_pointer<T*>::value` 与 `is_pointer<T*>::value` 不是同一个实例。
        MangleType::NtExpr(op, operands) => {
            let mut s = format!("nt_{}", op.key());
            for a in operands {
                s.push('_');
                s.push_str(&type_key(a));
            }
            s
        },
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

    /// 注册模板声明, 名字按**书写形态**登记 (有前缀则存限定名)。
    ///
    /// 命名空间里的模板在 AST 里只带短名 (`namespace std { struct is_pointer }`
    /// 的类名是 `is_pointer`), 而使用点可能写 `std::is_pointer<T*>::value`, 也可能
    /// 在同一个命名空间里写短名。注册表只能有**一个**实体名: 实例类名与缓存 key
    /// 都由名字派生, 同一声明登记两份就会分裂成两个类型实体。因此这里按声明所在
    /// 命名空间登记一个规范名, 使用点再由 [`Self::canonical_name`] 归一。
    pub fn register_with_prefix(
        &mut self,
        prefix: &str,
        targs: &TemplateArgs,
        requires: &Option<String>,
        decl: &CppDecl,
    ) {
        if prefix.is_empty() {
            self.register(targs, requires, decl);
            return;
        }
        if let Some(mut td) = Self::decl_from(targs, requires, decl.clone()) {
            if !td.name.contains("::") {
                td.name = format!("{prefix}::{}", td.name);
            }
            let fp = td.fingerprint();
            self.templates.retain(|t| t.fingerprint() != fp);
            self.templates.push(td);
        }
    }

    /// 把**使用点写的名字**归一到注册表里的规范名。
    ///
    /// `std::vector<int>` 与 `vector<int>` (`using namespace std` / 命名空间内的
    /// 递归引用) 是同一个类型, 必须得到同一个实例名与同一个缓存 key。归一规则:
    /// - 名字本身就是某个注册项 -> 原样返回;
    /// - 否则按短名找所有注册项, 要求它们**共享同一个命名空间前缀**
    ///   (`std::print` 的多个重载都来自 `std`) -> 归一到 `前缀::短名`;
    /// - 前缀不止一个 (`ns_a::vector` 与 `ns_b::vector`) 时**不猜**: 短名使用点
    ///   本来就不明确, 误归一会把两个不相关模板合并成一个候选集。
    pub fn canonical_name(&self, name: &str) -> String {
        if self.templates.iter().any(|t| t.name == name) {
            return name.to_string();
        }
        let short = short_name(name);
        let mut prefixes: Vec<&str> = Vec::new();
        for t in &self.templates {
            if short_name(&t.name) != short {
                continue;
            }
            if let Some(pos) = t.name.rfind("::") {
                let p = &t.name[..pos];
                if !prefixes.contains(&p) {
                    prefixes.push(p);
                }
            }
        }
        match prefixes.as_slice() {
            [only] => {
                // 使用点自己带了前缀时必须是同一个前缀 (`a::print` 不能归到 `std::print`)
                let want = name.strip_suffix(short).unwrap_or("").trim_end_matches("::");
                if want.is_empty() || want == *only {
                    format!("{only}::{short}")
                } else {
                    name.to_string()
                }
            },
            _ => name.to_string(),
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
        class: &crate::cpp::parser::ClassDecl,
    ) {
        self.full_specs.push(FullSpecEntry {
            name,
            specific_args,
            class: class.clone(),
        });
    }

    /// 查找模板 (按名)
    pub fn find(&self, name: &str) -> Option<&TemplateDecl> {
        let canon = self.canonical_name(name);
        self.templates.iter().find(|t| t.name == canon)
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
        // [temp.arg.nontype]: 与实例化同源的前提是先折叠依赖常量子式
        let name = self.canonical_name(name);
        let args = self.fold_args(args);
        let mut keys = vec![Self::key_for(&name, &args)];
        if let Some(td) = self.find(&name) {
            if let Some(resolved) = self.resolve_args(td, &args) {
                let rk = Self::key_for(&name, &resolved);
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
    ///
    /// 名字先过 [`Self::canonical_name`]: 限定名与短名两种书写必须看到同一个候选集。
    pub fn candidates_named(&self, name: &str) -> Vec<(usize, &TemplateDecl)> {
        let canon = self.canonical_name(name);
        self.templates
            .iter()
            .enumerate()
            .filter(|(_, t)| t.name == canon)
            .collect()
    }

    /// M51: 是否存在某个函数/类模板注册表项可作为隐式推导候选
    pub fn has_template_named(&self, name: &str) -> bool {
        let canon = self.canonical_name(name);
        self.templates.iter().any(|t| t.name == canon)
    }

    /// M51: 该名字是否**只**由函数模板承载 (至少一个函数模板候选, 且无类模板同名).
    ///
    /// 用于"无匹配函数"硬报错的门控: 类模板名出现在调用位置
    /// (`std::vector<int>(other)` 这种临时量构造 / 函数式类型转换) 不是
    /// 函数调用, 按候选决议失败报错会把合法代码拒之门外.
    pub fn only_fn_templates_named(&self, name: &str) -> bool {
        let canon = self.canonical_name(name);
        let mut saw_fn = false;
        for t in &self.templates {
            if t.name != canon {
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
        self.concept_params
            .insert(name.to_string(), params.to_vec());
    }

    /// M51: 记录模板/概念定义锚点 (取首次出现的顶层位置)
    pub fn set_anchor(&mut self, name: &str, anchor: usize) {
        self.anchors.entry(name.to_string()).or_insert(anchor);
    }

    /// M51: 查询定义锚点 (未登记时返回 usize::MAX, 由调用方按"追加到末尾"处理)
    pub fn anchor_of(&self, name: &str) -> usize {
        // 锚点按**限定名**登记 (见 `collect_templates_from_decl`), 短名使用点
        // 必须先归一, 否则实例会被插到错误的位置 (下游看不到字段类型定义)。
        let canon = self.canonical_name(name);
        self.anchors
            .get(name)
            .copied()
            .or_else(|| self.anchors.get(&canon).copied())
            .unwrap_or(usize::MAX)
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
            if !is_ident_start(bytes[i])
                || (i > 0 && (is_ident_cont(bytes[i - 1]) || bytes[i - 1] == b':'))
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
        // [temp.arg.nontype]: 依赖常量子式先进入解析后的实参表, 后面的默认实参
        // 填充 / requires 求值 / 实例 key 才都看到同一个值
        let args = self.fold_args(args);
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
                let packed =
                    if end == ai + 1 && matches!(args.get(ai), Some(MangleType::VariadicPack(_))) {
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
                },
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
                Some(prev) if prev == a => {},
                Some(_) => return None,
            }
        }
        found
    }

    /// M51: 显式模板实参规范化: 填充默认实参 + 求值 `requires`.
    /// 返回 `(候选下标, 实参表)`; None 表示该名字下所有候选都不可行
    /// (约束不满足 / 实参过多或不足且无默认), 调用方必须按 SFINAE 静默跳过,
    /// 不得报硬错误.
    ///
    /// [temp.deduct]/8: 除了 `requires` 不满足, **依赖名在代入后不存在**也是
    /// 替换失败 (`enable_if<false, int>::type` 里没有 `type`)。那个失败发生在
    /// 签名上, 不报到这里就会变成下游的 `unknown class type` 硬错。
    pub fn normalize_args(
        &self,
        name: &str,
        args: &[MangleType],
    ) -> Option<(usize, Vec<MangleType>)> {
        self.candidates_named(name)
            .into_iter()
            .find_map(|(i, td)| {
                let r = self.resolve_args(td, args)?;
                if !self.signature_is_viable(td, &r) {
                    return None;
                }
                Some((i, r))
            })
    }

    /// [temp.deduct]/8: 候选的**签名** (返回类型 + 形参类型 + 被用到的默认实参)
    /// 代入实参后, 其中的依赖名 `X<Args>::member` 必须能在选中的特化体里找到;
    /// 找不到则该候选不可行 (SFINAE 剔除), 而不是拉到下游报错。
    ///
    /// 拿不定主意时一律判“可行” (保持旧行为): 只有“确实看到了那个类体但没有
    /// 这个成员”才是替换失败; 类模板根本不在注册表里 (模板模板形参占位等)
    /// 不是失败。
    fn signature_is_viable(&self, td: &TemplateDecl, args: &[MangleType]) -> bool {
        let mut sig: Vec<&MangleType> = Vec::new();
        match &td.decl {
            CppDecl::FreeFn { ret, params, .. } => {
                sig.push(ret);
                sig.extend(params.iter().map(|(_, t)| t));
            },
            // 类模板的嵌套名误用会在引用点报出来, 这里不剪候选
            _ => return true,
        }
        for default in td.defaults.iter().flatten() {
            sig.push(default);
        }
        for ty in sig {
            let substituted = substitute_type(ty, &td.param_names, args);
            if !self.dependent_names_exist(&substituted, 0) {
                return false;
            }
        }
        true
    }

    /// 类型树里出现的每个 `X<Args>::member` 都必须存在 (递归)。
    fn dependent_names_exist(&self, ty: &MangleType, depth: u32) -> bool {
        if depth >= MAX_FOLD_DEPTH {
            return true;
        }
        match ty {
            MangleType::TplMember { name, args, member } => {
                self.class_member_exists(name, args, member, depth)
                    && args.iter().all(|a| self.dependent_names_exist(a, depth + 1))
            },
            MangleType::Template { args, .. } => {
                args.iter().all(|a| self.dependent_names_exist(a, depth + 1))
            },
            MangleType::NtExpr(_, operands) => {
                operands.iter().all(|a| self.dependent_names_exist(a, depth + 1))
            },
            MangleType::Pointer(t) | MangleType::Reference(t) | MangleType::Array(t, _) => {
                self.dependent_names_exist(t, depth + 1)
            },
            MangleType::VariadicPack(p) => p.iter().all(|a| self.dependent_names_exist(a, depth + 1)),
            MangleType::Function { ret, params } => {
                self.dependent_names_exist(ret, depth + 1)
                    && params.iter().all(|a| self.dependent_names_exist(a, depth + 1))
            },
            _ => true,
        }
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
            .find(|i| {
                self.inst_cache
                    .contains_key(&Self::fn_inst_key(*i, name, args))
            })
    }

    /// 实例化产物的统一收尾: **参数包摊平** (见 `pack_expand` 模块).
    ///
    /// 失败时保留原树但记下原因 (`pack_expand_error`), 由调用端 (native 后端 /
    /// lower) 硬报错 —— 静默交出去就会在背深处变成莫名的符号错误.
    fn finish_inst(
        &mut self,
        mut inst: CppDecl,
        params: &[String],
        args: &[MangleType],
    ) -> CppDecl {
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
    pub fn instantiate_fn_at(
        &mut self,
        idx: usize,
        name: &str,
        args: &[MangleType],
    ) -> Option<CppDecl> {
        let canon = self.canonical_name(name);
        let name = canon.as_str();
        let folded = self.fold_args(args);
        let args = folded.as_slice();
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
        self.candidates_named(name)
            .into_iter()
            .find_map(|(i, td)| {
                let r = self.deduce_args(td, call_arg_tys)?;
                // [temp.deduct]/8: 推导成功但签名里的依赖名代入后不存在 —— 替换
                // 失败, 该候选从候选集剔除 (下一个候选仍可能被选中)
                if !self.signature_is_viable(td, &r) {
                    return None;
                }
                Some((i, r))
            })
    }

    /// G13: 实例化模板类 - 优先匹配全特化 > 偏特化 > 主模板
    pub fn instantiate_class(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        // [temp.arg.nontype]: 实参表里的依赖常量子式先折成 `NonType` 再匹配特化
        let canon = self.canonical_name(name);
        let name = canon.as_str();
        let folded = self.fold_args(args);
        let args = folded.as_slice();
        // First try full specialization (exact match)
        if let Some(full) = self.find_full_spec(name, args) {
            // 全特化体已具体, 无需代入形参; 但实例名仍按主模板路径的
            // `instantiated_class_name` 生成 —— 同一个 `X<int>` 在程序里只能
            // 有一个类实体, 用与主模板实例化相同的命名才不会和
            // `instance_class_name` / 引用点的名字漂移.
            let key = Self::key_for(name, args);
            if let Some(cached) = self.inst_cache.get(&key) {
                return Some(cached.clone());
            }
            let inst = self.full_class_to_decl(&full.class, &instantiated_class_name(name, args));
            self.inst_cache.insert(key.clone(), inst.clone());
            self.inst_order.push(key);
            return Some(inst);
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
        let canon = self.canonical_name(name);
        let name = canon.as_str();
        let folded = self.fold_args(args);
        let args = folded.as_slice();
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
        let name = self.canonical_name(name);
        let name = name.as_str();
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

    /// M55: 查已实例化类 `name<args>` 中的嵌套 **typedef 成员** `member`,
    /// 返回它的目标类型.
    ///
    /// `enable_if<B,T>::type` / `iterator_traits<P>::value_type` / `traits_type::int_type`
    /// 这类**依赖类型名**在 AST 里是 `TplMember`, 它的含义是“取实例类里的一个
    /// 类型别名”, 而不是“名为 `inst::type` 的类”。实例必须已由 `instantiate_class`
    /// 进入 `inst_cache` (引用点的 `collect_template_refs_*` 会触发实例化)。
    pub fn instance_typedef(
        &self,
        name: &str,
        args: &[MangleType],
        member: &str,
    ) -> Option<MangleType> {
        // [temp.arg.nontype]: 实参表折叠后才能命中 `enable_if<true, T>` 这类实例
        let name = self.canonical_name(name);
        let args = self.fold_args(args);
        let inst = self.inst_cache.get(&Self::key_for(&name, &args))?;
        let CppDecl::Class(c) = inst else {
            return None;
        };
        c.members.iter().find_map(|m| match m {
            CppDecl::Typedef { name: tn, target } if tn == member => Some(target.clone()),
            _ => None,
        })
    }

    /// G13: 查找全特化：精确匹配优先
    pub fn find_full_spec(&self, name: &str, args: &[MangleType]) -> Option<&FullSpecEntry> {
        let canon = self.canonical_name(name);
        let name = canon.as_str();
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
    /// G13: 将全特化体展开成一个具名类声明.
    ///
    /// 只重命名 (主模板名 -> 实例名), 基类 / 全部成员 / 访问级别原样搬运:
    /// 特化体就是一个完整的类, 不存在“从主模板继承布局”这回事.
    fn full_class_to_decl(&self, class: &ClassDecl, inst_name: &str) -> CppDecl {
        let mut out = class.clone();
        out.name = inst_name.to_string();
        CppDecl::Class(out)
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

    /// 登记一个**普通** (非模板) 类的编译期常量与基类链, 连带它的嵌套类。
    ///
    /// `prefix` 是外层命名空间/类的限定前缀 (可为空)。同时以**完整限定名**与
    /// **剥掉前缀的末段名**两个键登记: 基类子句和 `Cls::value` 里的写法可能带
    /// `std::` 也可能不带 (取决于使用点的 `using`/重开声明), 末段冲突时保留先
    /// 登记者 —— 完整限定键始终优先命中, 不会误读。
    pub fn index_plain_class(&mut self, prefix: &str, c: &crate::cpp::parser::ClassDecl) {
        let info = plain_class_info(c);
        let qualified = if prefix.is_empty() {
            c.name.clone()
        } else {
            format!("{}::{}", prefix, c.name)
        };
        for key in [qualified.clone(), c.name.clone()] {
            // 末段名只在尚未登记时写入: 同一个短名在不同命名空间里属于两个类,
            // 先登记者赢 (完整限定键仍然准确命中, 回退键只是便利)。
            self.plain_classes.entry(key).or_insert_with(|| info.clone());
        }
        for m in &c.members {
            if let CppDecl::Class(inner) = m {
                self.index_plain_class(&qualified, inner);
            }
        }
    }

    /// 登记一个 **typedef / using 别名**。
    ///
    /// 只登记**命名空间/全局作用域**的别名: 类体内的 `typedef T type;` 是**依赖名**
    /// (它的含义取决于实例化后的实参), 已由 `collect_member_names` 记在类体的成员名
    /// 表里; 把它当全局别名登记会让所有 `X::type` 共用一个目标类型, SFINAE 判据反而
    /// 失真。键名规则与 [`Self::index_plain_class`] 一致 (限定名 + 末段名, 末段先登记者赢)。
    pub fn index_type_alias(&mut self, prefix: &str, name: &str, target: &MangleType) {
        let qualified = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}::{name}")
        };
        for key in [qualified, name.to_string()] {
            self.type_aliases.entry(key).or_insert_with(|| target.clone());
        }
    }

    /// 查一个名字是否是别名 (完整书写优先, 末段名回退)。
    fn type_alias(&self, name: &str) -> Option<&MangleType> {
        self.type_aliases
            .get(name)
            .or_else(|| self.type_aliases.get(short_name(name)))
    }

    /// 别名目标类上的编译期常量 (`true_type::value` → `integral_constant<bool,1>::value`)。
    fn alias_member_constant(&self, target: &MangleType, member: &str, depth: u32) -> Option<i64> {
        if depth >= MAX_FOLD_DEPTH {
            return None;
        }
        match target {
            MangleType::Class(n) => self
                .plain_class_const(n, member, depth + 1)
                .or_else(|| self.class_template_const(n, &[], member, depth + 1)),
            MangleType::Template { name, args } => {
                self.class_template_const(name, args, member, depth + 1)
            },
            // 别名指向标量 (`typedef int myint;`) 时没有成员可取
            _ => None,
        }
    }

    /// 别名目标类里是否有名为 `member` 的成员 (递回走基类链与嵌套别名)。
    fn alias_has_member(&self, target: &MangleType, member: &str, depth: u32) -> bool {
        if depth >= MAX_FOLD_DEPTH {
            return true;
        }
        match target {
            MangleType::Class(n) => self.base_has_member(n, &[], member, depth + 1),
            MangleType::Template { name, args } => {
                self.class_member_exists(name, args, member, depth + 1)
            },
            // 目标类型看不出类体 (标量别名等): 拿不定主意不剪候选
            _ => true,
        }
    }

    /// 普通类 (含沿基类链上溯) 的编译期常量成员。
    fn plain_class_const(&self, class_name: &str, member: &str, depth: u32) -> Option<i64> {
        if depth >= MAX_FOLD_DEPTH {
            return None;
        }
        // 完整书写名优先, 拿不到再按短名回退: 同一个类在基类子句里可能带
        // `std::` 前缀, 在另一个使用点却不带 (取决于 `using namespace`)。
        let info = self
            .plain_classes
            .get(class_name)
            .or_else(|| self.plain_classes.get(short_name(class_name)));
        if let Some(info) = info {
            if let Some((_, v)) = info.consts.iter().find(|(n, _)| n == member) {
                return Some(*v);
            }
            for (base, base_args) in &info.bases {
                let v = if base_args.is_empty() {
                    self.plain_class_const(base, member, depth + 1)
                } else {
                    self.class_template_const(base, base_args, member, depth + 1)
                };
                if v.is_some() {
                    return v;
                }
            }
            return None;
        }
        // 名字不是类名而是 typedef 别名 (`: true_type`) —— 先把名字还原成目标类,
        // 否则继承链在这里断掉, `is_pointer<T*>::value` 折不出常量。
        if let Some(target) = self.type_alias(class_name) {
            let target = target.clone();
            return self.alias_member_constant(&target, member, depth);
        }
        None
    }

    /// [class.member.lookup]: 给定类体取常量 —— 先本类成员, 再沿基类链上溯。
    ///
    /// traits 的 `value` 几乎总是**继承**来的 (`struct is_pointer<T*> : true_type {}`),
    /// 所以上溯不是可选项。
    fn class_decl_const(&self, c: &crate::cpp::parser::ClassDecl, member: &str, depth: u32) -> Option<i64> {
        if depth >= MAX_FOLD_DEPTH {
            return None;
        }
        let info = plain_class_info(c);
        if let Some((_, v)) = info.consts.iter().find(|(n, _)| n == member) {
            return Some(*v);
        }
        for (base, base_args) in &info.bases {
            let v = if base_args.is_empty() {
                // 基类名空实参时两种可能: 普通类 (`: true_type`), 或零形参类模板。
                self.plain_class_const(base, member, depth + 1)
                    .or_else(|| self.class_template_const(base, &[], member, depth + 1))
            } else {
                self.class_template_const(base, base_args, member, depth + 1)
            };
            if v.is_some() {
                return v;
            }
        }
        None
    }

    /// `name<args>::member` 的编译期常量求值: 无实参时先按普通类查, 再按类模板
    /// 实例查 (两条路径在 [temp.arg.nontype] 折叠里都要走: `std::true_type::value`
    /// 与 `is_pointer<int*>::value` 是同一个书写形态)。
    fn class_member_constant(
        &self,
        name: &str,
        args: &[MangleType],
        member: &str,
        depth: u32,
    ) -> Option<i64> {
        if depth >= MAX_FOLD_DEPTH {
            return None;
        }
        if args.is_empty() {
            if let Some(v) = self.plain_class_const(name, member, depth + 1) {
                return Some(v);
            }
        }
        self.class_template_const(name, args, member, depth + 1)
    }

    /// 已实例化类体 / 匹配到的特化体里的常量成员取值 (`X<Args>::value`)。
    fn class_template_const(
        &self,
        name: &str,
        args: &[MangleType],
        member: &str,
        depth: u32,
    ) -> Option<i64> {
        if depth >= MAX_FOLD_DEPTH {
            return None;
        }
        let name = self.canonical_name(name);
        let name = name.as_str();
        // 折叠后的实参才是与实例化同源的 key (嵌套子式可能还未折)
        let args = self.fold_args_inner(args, depth);
        if let Some(CppDecl::Class(c)) = self.inst_cache.get(&Self::key_for(name, &args)) {
            if let Some(v) = self.class_decl_const(c, member, depth + 1) {
                return Some(v);
            }
        }
        let (decl, params, bound) = self.matched_class_body(name, &args);
        if params.is_empty() {
            return match decl {
                CppDecl::Class(c) => self.class_decl_const(&c, member, depth + 1),
                _ => None,
            };
        }
        // 特化体可能仍带形参 (`: integral_constant<bool, v>`), 代入主模板实参后取值。
        match decl {
            CppDecl::Class(c) => {
                let c = substitute_class_consts(&c, &params, &bound);
                self.class_decl_const(&c, member, depth + 1)
            },
            _ => None,
        }
    }

    /// [temp.deduct]/8 判据: `name<args>::member` 代入后是否真的存在。
    ///
    /// 存在 = 命中的类体 (已实例化的优先, 它携带代入后的成员) 或它的**基类**里有
    /// 同名成员 (`traits<T>::value_type` 来于基类是常见写法)。
    /// 拿不定主意时返 true: 只有“确实看到类体但里面没这个成员”才是替换失败。
    pub fn class_member_exists(
        &self,
        name: &str,
        args: &[MangleType],
        member: &str,
        depth: u32,
    ) -> bool {
        if depth >= MAX_FOLD_DEPTH {
            return true;
        }
        let cname = self.canonical_name(name);
        let name = cname.as_str();
        let args = self.fold_args_inner(args, depth);
        if args.is_empty() {
            if let Some(target) = self.type_alias(name) {
                let target = target.clone();
                return self.alias_has_member(&target, member, depth);
            }
        }
        if let Some(CppDecl::Class(c)) = self.inst_cache.get(&Self::key_for(name, &args)) {
            return self.class_decl_has_member(c, member, depth);
        }
        match self.matched_class_body(name, &args) {
            // 名字不在注册表 (模板模板形参占位等): 不剪候选
            (CppDecl::Empty, _, _) => true,
            (CppDecl::Class(c), _, _) => self.class_decl_has_member(&c, member, depth),
            _ => true,
        }
    }

    /// 类体 (含沿基类链) 是否有名为 `member` 的成员。
    fn class_decl_has_member(
        &self,
        c: &crate::cpp::parser::ClassDecl,
        member: &str,
        depth: u32,
    ) -> bool {
        if depth >= MAX_FOLD_DEPTH {
            return true;
        }
        let info = plain_class_info(c);
        if info.member_names.iter().any(|n| n == member) {
            return true;
        }
        info.bases
            .iter()
            .any(|(base, base_args)| self.base_has_member(base, base_args, member, depth + 1))
    }

    /// 基类 (普通类或类模板) 里是否有某成员。基类**定义查不到**时返 true ——
    /// 拿不定主意不剪候选 (前向声明的基类 / 模板模板形参占位)。
    fn base_has_member(
        &self,
        base: &str,
        base_args: &[MangleType],
        member: &str,
        depth: u32,
    ) -> bool {
        if depth >= MAX_FOLD_DEPTH {
            return true;
        }
        if !base_args.is_empty() {
            return self.class_member_exists(base, base_args, member, depth);
        }
        match self
            .plain_classes
            .get(base)
            .or_else(|| self.plain_classes.get(short_name(base)))
        {
            Some(info) => {
                info.member_names.iter().any(|n| n == member)
                    || info
                        .bases
                        .iter()
                        .any(|(nb, ab)| self.base_has_member(nb, ab, member, depth + 1))
            },
            None => {
                // 基类名是个 typedef 别名 (`struct is_pointer<T*> : true_type {}`):
                // 必须展开到目标类再查, 否则“成员是否存在”只能答“不知道”。
                match self.type_alias(base) {
                    Some(target) => {
                        let target = target.clone();
                        self.alias_has_member(&target, member, depth)
                    },
                    None => true,
                }
            },
        }
    }

    /// 实参 `name<args>` 命中的类体, 以及**该体自己的**形参名/对应绑定。
    ///
    /// 主模板路径的形参与实参一一对应; 长度不等时 (还有默认实参未填,
    /// 而 `resolve_args` 在折叠路径里不可用 —— 它会反过来再走一次折叠)
    /// 不做代入, 只认类体里的字面量常量。
    fn matched_class_body(
        &self,
        name: &str,
        args: &[MangleType],
    ) -> (CppDecl, Vec<String>, Vec<MangleType>) {
        match self.get_best_match(name, args) {
            BestMatchResult::FullSpec(full) => {
                (CppDecl::Class(full.class.clone()), Vec::new(), Vec::new())
            },
            BestMatchResult::PartialSpec(_) => match self.pick_partial(name, args) {
                // 偏特化体自带一套形参 (`enable_if<true, T>` 只有 `T`), 绑定由
                // `pick_partial` 给出; 两者长度一致才能代入
                Some((decl, names, bound)) if bound.len() == names.len() => (decl, names, bound),
                _ => (CppDecl::Empty, Vec::new(), Vec::new()),
            },
            BestMatchResult::Primary(td) => {
                let params = td.param_names.clone();
                if !params.is_empty() && params.len() == args.len() {
                    (td.decl.clone(), params, args.to_vec())
                } else {
                    (td.decl.clone(), Vec::new(), Vec::new())
                }
            },
            BestMatchResult::NotFound => (CppDecl::Empty, Vec::new(), Vec::new()),
        }
    }

    /// 把模板**实参表**里的编译期常量子式折成 `MangleType::NonType`。
    ///
    /// [temp.arg.nontype] 规定非类型实参是一个常量表达式, 而 traits 惯用法把它写成
    /// 类作用域的限定名 (`enable_if<is_pointer<T>::value, int>` 的首个实参)。AST 里
    /// 它是 `TplMember { name: "is_pointer", args: […], member: "value" }`, 不先折成
    /// 常量就参与实例化会两处同时失效:
    /// - 偏特化匹配比的是 key (`tm_is_pointer_p_int_value` 对比 pattern `true`),
    ///   `enable_if<true, T>` 永远选不中 -> 实例只剩主模板 (没有 `type` 别名);
    /// - 实例名把整个子式当类名编码进去
    ///   (`__dcc_template_enable_if_tm_is_pointer_p_int_value_int`)。
    ///
    /// 所有以实参表为输入的公开入口 (key / 匹配 / 命名 / typedef 展开) 都先过这一道
    /// 折叠, 保证三者同源。折叠幂等: 折成 `NonType` 后不再触发查找。
    pub fn fold_args(&self, args: &[MangleType]) -> Vec<MangleType> {
        args.iter().map(|a| self.fold_arg(a, 0)).collect()
    }

    /// 折叠**单个**类型/实参形态 (`X<Args>::member` 是常量时得 `NonType`,
    /// 依赖子式叶子全成常量时也一样)。用于下游展开嵌套名时区分“值”与“类”。
    pub fn fold_constant(&self, ty: &MangleType) -> MangleType {
        self.fold_arg(ty, 0)
    }

    fn fold_args_inner(&self, args: &[MangleType], depth: u32) -> Vec<MangleType> {
        args.iter().map(|a| self.fold_arg(a, depth + 1)).collect()
    }

    /// 折叠单个实参; 类型层级 (`Pointer`/`Reference`/`Array`/函数类型/参数包/
    /// 嵌套 `Template`) 里的子实参同样递归处理。
    fn fold_arg(&self, ty: &MangleType, depth: u32) -> MangleType {
        if depth >= MAX_FOLD_DEPTH {
            return ty.clone();
        }
        match ty {
            MangleType::TplMember { name, args, member } => {
                let inner = self.fold_args_inner(args, depth);
                if let Some(v) = self.class_member_constant(name, &inner, member, depth) {
                    return MangleType::NonType(v);
                }
                MangleType::TplMember {
                    name: name.clone(),
                    args: inner,
                    member: member.clone(),
                }
            },
            MangleType::Template { name, args } => MangleType::Template {
                name: name.clone(),
                args: self.fold_args_inner(args, depth),
            },
            MangleType::Pointer(t) => MangleType::Pointer(Box::new(self.fold_arg(t, depth + 1))),
            MangleType::Reference(t) => MangleType::Reference(Box::new(self.fold_arg(t, depth + 1))),
            MangleType::Array(t, n) => MangleType::Array(Box::new(self.fold_arg(t, depth + 1)), *n),
            MangleType::VariadicPack(p) => MangleType::VariadicPack(self.fold_args_inner(p, depth)),
            MangleType::Function { ret, params } => MangleType::Function {
                ret: Box::new(self.fold_arg(ret, depth + 1)),
                params: self.fold_args_inner(params, depth),
            },
            // [temp.dep.constexpr]: 依赖子式的叶子先折 (逐级代入实例常量), 全部
            // 叶子都成了 `NonType` 才能求运算值; 否则保留结构 (下一次再试)。
            MangleType::NtExpr(op, operands) => {
                let inner = self.fold_args_inner(operands, depth);
                if let Some(v) = eval_nt_expr(*op, &inner) {
                    return MangleType::NonType(v);
                }
                MangleType::NtExpr(*op, inner)
            },
            other => other.clone(),
        }
    }
}

/// 常量折叠 / 继承链上溯的深度上限 (防御性: 递归型 traits 不能把编译器带进环)
const MAX_FOLD_DEPTH: u32 = 8;

/// 把类体里的**模板形参**按名->实参代入 (常量求值用)。两个位置必须同时代入:
/// - 基类子句 (`: integral_constant<bool, v>` 里的 `v` 还是形参名, 不代入就
///   匹配不到已登记的实例);
/// - 静态数据成员的类内初值 (`static const T value = v;` 的 `v`)。这一项是
///   libstdc++/libc++ 里 `integral_constant<T, v>::value` 的**唯一**取值路径:
///   不代入则初值是个裸形参名, 折不出常量, 所有继承它的 traits 都得不到 `value`。
fn substitute_class_consts(
    c: &crate::cpp::parser::ClassDecl,
    params: &[String],
    args: &[MangleType],
) -> crate::cpp::parser::ClassDecl {
    let mut out = c.clone();
    for b in &mut out.bases {
        if let Some(ta) = &mut b.template_args {
            *ta = ta.iter().map(|t| substitute_type(t, params, args)).collect();
        }
    }
    for m in &mut out.members {
        if let CppDecl::MemberVar {
            init: Some(old),
            is_static: true,
            ..
        } = m
        {
            let sub = substitute_expr(old, params, args);
            *old = sub;
        }
    }
    out
}

/// 折叠一个类体的编译期常量与基类子句
fn plain_class_info(c: &crate::cpp::parser::ClassDecl) -> PlainClassInfo {
    let mut info = PlainClassInfo::default();
    for b in &c.bases {
        info.bases.push((
            b.name.clone(),
            b.template_args.clone().unwrap_or_default(),
        ));
    }
    collect_class_consts(&c.members, &mut info.consts);
    collect_member_names(&c.members, &mut info.member_names);
    info
}

/// 类体引入的成员名 ([temp.deduct]/8 只问“有没有这个成员”)。
fn collect_member_names(members: &[CppDecl], out: &mut Vec<String>) {
    for m in members {
        match m {
            CppDecl::Typedef { name, .. } => out.push(name.clone()),
            CppDecl::Class(inner) => out.push(inner.name.clone()),
            CppDecl::MemberVar { name, .. } | CppDecl::GlobalVar { name, .. } => {
                out.push(name.clone())
            },
            CppDecl::MemberFn { name, .. } => out.push(name.clone()),
            CppDecl::Enum { name, variants, .. } => {
                // 匿名 enum (`enum { value = 1 }`) 本身不引入类名, 但枚举量引入
                if !name.is_empty() && name != "_anon" {
                    out.push(name.clone());
                }
                out.extend(variants.iter().map(|(n, _)| n.clone()));
            },
            CppDecl::Template { decl, .. } => {
                if let CppDecl::Class(inner) = &**decl {
                    out.push(inner.name.clone());
                }
            },
            _ => {},
        }
    }
}

/// 类体成员里的整数常量: `enum { value = N }` 与 `static const/constexpr` 数据成员。
fn collect_class_consts(members: &[CppDecl], out: &mut Vec<(String, i64)>) {
    for m in members {
        match m {
            CppDecl::Enum { variants, .. } => {
                // 与 parser 的枚举求值同一规则: 首个无初值为 0, 之后递增;
                // 显式初值折不成字面量时不猜值 (跳过该枚举量)。
                let mut next: i64 = 0;
                for (name, init) in variants {
                    let v = match init {
                        Some(text) => match const_int_from_text(text) {
                            Some(x) => x,
                            None => continue,
                        },
                        None => next,
                    };
                    out.push((name.clone(), v));
                    next = v.saturating_add(1);
                }
            },
            // 类内**静态**数据成员: `static const int N = 7;` / `static constexpr bool
            // value = true;` (libstdc++ 的 `integral_constant<T, v>::value` 就长这样)。
            // 只有 static 的才是类作用域常量: 非静态成员的类内初值属于每个子对象的
            // 构造过程, 当常量用会让同一个 `value` 对所有实例返回同一个数。
            CppDecl::MemberVar {
                name,
                is_static: true,
                init: Some(init),
                ..
            } => {
                if let Some(v) =
                    crate::cpp::cpp_const_eval::eval_cpp_const(init, &Default::default())
                {
                    out.push((name.clone(), v as i64));
                }
            },
            // 类体解析入口也可能拿到 namespace 作用域的常量声明 (`constexpr int k = 3;`),
            // 一并收下 (与静态成员同名时上面的分支优先命中)。
            CppDecl::GlobalVar {
                name,
                init: Some(init),
                ..
            } => {
                if let Some(v) =
                    crate::cpp::cpp_const_eval::eval_cpp_const(init, &Default::default())
                {
                    out.push((name.clone(), v as i64));
                }
            },
            _ => {},
        }
    }
}

/// 枚举量初值文本 -> 整数。parser 已把可折叠的初值 (`square(3)` / 已登记常量)
/// 改写成字面量, 故这里只认字面量与 bool 字面量 (含 `u`/`l` 后缀)。
fn const_int_from_text(s: &str) -> Option<i64> {
    let t = s.trim();
    match t {
        "true" => return Some(1),
        "false" => return Some(0),
        _ => {},
    }
    let t = t.trim_end_matches(['u', 'U', 'l', 'L']);
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).ok();
    }
    t.parse::<i64>().ok()
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
        // [temp.dep.constexpr]: 依赖非类型子式的**操作数**里同样带着形参名
        // (`!is_pointer<T>::value` 的 `T`), 不代入就永远折不出值。
        MangleType::NtExpr(op, operands) => {
            MangleType::NtExpr(*op, operands.iter().map(sub).collect())
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
        // [temp.dep.type]: 表达式语境里的模板 id 也要把形参名换成实参。以前这两个
        // 形态没有分支, `is_pointer<T>::value` 在 `ptr_rank<int>` 与 `ptr_rank<int*>`
        // 里保持同一份未替换的实参, 于是两个实例读到同一个常量。
        CppExpr::TplStaticAccess {
            name,
            args: targs,
            member,
        } => CppExpr::TplStaticAccess {
            name: name.clone(),
            args: targs.iter().map(|a| substitute_type(a, params, args)).collect(),
            member: member.clone(),
        },
        CppExpr::TemplateIdent { name, args: targs } => CppExpr::TemplateIdent {
            name: name.clone(),
            args: targs.iter().map(|a| substitute_type(a, params, args)).collect(),
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
                    _ => {},
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
            is_const,
        } => {
            // M53: 局部变量**遮蔽**同名模板形参时整条不再替换 (`int N = 1;` 的
            // 初始化式按 C++ 作用域规则已经指向新变量)。
            if params.iter().any(|p| p == name) {
                return CppStmt::LocalVar {
                    ty: ty.clone(),
                    name: name.clone(),
                    init: init.clone(),
                    is_volatile: *is_volatile,
                    is_const: *is_const,
                };
            }
            CppStmt::LocalVar {
                ty: substitute_type(ty, params, args),
                name: name.clone(),
                init: init.as_ref().map(|x| substitute_expr(x, params, args)),
                is_volatile: *is_volatile,
                is_const: *is_const,
            }
        },
        CppStmt::StaticLocal {
            ty,
            name,
            init,
            is_volatile,
            is_const,
        } => CppStmt::StaticLocal {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            init: init.as_ref().map(|x| substitute_expr(x, params, args)),
            is_volatile: *is_volatile,
            is_const: *is_const,
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
                    is_const: spec.is_const,
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
            is_static,
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
                is_static: *is_static,
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
                        .map(|i| crate::cpp::parser::CppCtorInit {
                            name: i.name.clone(),
                            args: i
                                .args
                                .iter()
                                .map(|e| substitute_expr(e, &bp, &ba))
                                .collect(),
                            braces: i.braces,
                        })
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
            is_const,
            is_static,
            init,
        } => CppDecl::MemberVar {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            is_mutable: *is_mutable,
            is_volatile: *is_volatile,
            is_const: *is_const,
            // 静态性与类内初值都是声明的一部分: 丢掉 `static const T value = v;`
            // 的初值就让 traits 的常量再也折不出来。初值本身也是**依赖表达式**,
            // 必须与类型同步代入 (`v` -> 实参值, `N * 2` -> `7 * 2`), 否则实例体里
            // 留下裸形参名, sema 求不出常量 (报 `identifier ... is not a value`)。
            is_static: *is_static,
            init: init.as_ref().map(|e| substitute_expr(e, params, args)),
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
                    is_const: false,
                    is_static: false,
                    init: None,
                },
                CppDecl::Ctor {
                    class: "Box".to_string(),
                    params: vec![("v".to_string(), MangleType::Class("T".to_string()))],
                    inits: vec![crate::cpp::parser::CppCtorInit::direct(
                        "payload",
                        vec![CppExpr::Ident("v".to_string())],
                    )],
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
                is_const: false,
                is_static: false,
                init: None,
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

        let result =
            reg.instantiate_class("reverse_iterator", std::slice::from_ref(&iterator_type));
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
        // [temp.dep.constexpr]: 依赖非类型子式的操作数递归代入
        MangleType::NtExpr(op, operands) => MangleType::NtExpr(
            *op,
            operands
                .iter()
                .map(|a| substitute_template_refs_in_type(a, bindings))
                .collect(),
        ),

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
                        is_const,
                        is_static,
                        init,
                    } => CppDecl::MemberVar {
                        ty: substitute_template_refs_in_type(ty, bindings),
                        name: name.clone(),
                        is_mutable: *is_mutable,
                        is_volatile: *is_volatile,
                        is_const: *is_const,
                        is_static: *is_static,
                        init: init.clone(),
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
        reg.register_concept("SameAs", &["A".to_string(), "B".to_string()], "is_same<A, B>::value");
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
            &fn_tpl(
                "tag",
                vec![("x".to_string(), MangleType::Class("T".to_string()))],
                MangleType::Int,
            ),
        );
        reg.register(
            &one,
            &None,
            &fn_tpl(
                "tag",
                vec![("y".to_string(), MangleType::Class("T".to_string()))],
                MangleType::Int,
            ),
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
            &fn_tpl(
                "sfinae_anon",
                vec![("x".to_string(), MangleType::Class("T".to_string()))],
                MangleType::Int,
            ),
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

// ===== Stage 8.1/8.4/8.5/8.6/8.7: 模板迁入 sema 辅助函数 =====

/// Stage 8.5: 缓存键 `class:<name><arg0,arg1,...>` 编码。
///
/// 与 `instantiated_class_name` 的命名规则保持一致, 只是在前面加 `class:` 前缀
/// 区分函数实例化缓存 (`fn:<name>` 前缀, 见 `type_key_for_template_fn`)。
pub fn type_key_for_template(name: &str, args: &[MangleType]) -> String {
    format!("class:{}", instantiated_class_name(name, args))
}

/// Stage 8.5: 缓存键 `fn:<name>(arg0,arg1,...)` 编码。
pub fn type_key_for_template_fn(name: &str, args: &[MangleType]) -> String {
    let mut s = format!("fn:{}(", name);
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&type_key(a));
    }
    s.push(')');
    s
}

/// Stage 8.4: 函数模板实例的 linkage 名 (`__dcc_template_fn_<name>_<arg-key>`)。
pub fn instantiated_fn_name(name: &str, args: &[MangleType]) -> String {
    let mut s = format!("__dcc_template_fn_{}", name);
    for a in args {
        s.push('_');
        s.push_str(&type_key(a));
    }
    s
}

/// Stage 8.6: 偏特化 pattern 特异性评分。
///
/// 返回值约定: 精确模式 (字面量匹配) = 3, 指针剥皮 (`T*`) = 2, 通配 = 1。
/// 与 [temp.spec.partial.match]/[temp.spec.partial.order] 同构。
pub fn score_partial_pattern(pattern_text: &str) -> usize {
    let p = pattern_text.trim();
    if p.is_empty() {
        return 1;
    }
    // 精确非类型实参 (`conditional<false, T, F>` 的 `false`)
    if matches!(p, "true" | "false") || p.parse::<i64>().is_ok() {
        return 3;
    }
    // 指针剥皮 (`T*`)
    if p.ends_with('*') && !p.contains(',') {
        return 2;
    }
    // 通配 (`T`)
    1
}

/// Stage 8.3: 把 `CxxType` 序列降级为 `MangleType` 序列 (用于函数模板实参推导)。
///
/// 失败的元素用 `MangleType::Class("<unknown>")` 占位, 保持 arity 一致, 让
/// 上游 `deduce_args` 仍能跑 ([temp.deduct]/8 的"全部静默移出"语义)。
///
/// 只覆盖 `CxxType` 的部分真实 variant: 内置类型 / Pointer / Reference / Array /
/// Class; Function / MemberPointer / Dependent / Alias / Enum 走 `__unknown__`。
pub fn cxx_type_to_mangle(ty: &crate::cpp::sema::CxxType) -> MangleType {
    use crate::cpp::sema::CxxType;
    match ty {
        CxxType::Void => MangleType::Void,
        CxxType::Bool => MangleType::Bool,
        CxxType::Integer { bits: 16, signed: false } => MangleType::Ushort,
        CxxType::Integer { bits: 32, signed: false } => MangleType::Uint,
        CxxType::Integer { bits: 64, signed: false } => MangleType::Ulong,
        CxxType::Integer { bits: 8, signed: true } => MangleType::Schar,
        CxxType::Integer { bits: 16, signed: true } => MangleType::Short,
        CxxType::Integer { bits: 32, signed: true } => MangleType::Int,
        CxxType::Integer { bits: 64, signed: true } => MangleType::Long,
        CxxType::Integer { .. } => MangleType::Int,
        CxxType::Float { bits: 32 } => MangleType::Float,
        CxxType::Float { bits: 64 } => MangleType::Double,
        CxxType::Float { .. } => MangleType::Double,
        CxxType::Pointer(t) => MangleType::Pointer(Box::new(cxx_type_to_mangle(t))),
        CxxType::LValueRef(t) | CxxType::RValueRef(t) => {
            MangleType::Reference(Box::new(cxx_type_to_mangle(t)))
        },
        CxxType::Array(t, n) => MangleType::Array(Box::new(cxx_type_to_mangle(t)), n.unwrap_or(0)),
        CxxType::Class(cid) => MangleType::Class(format!("cls_{}", cid.0)),
        _ => MangleType::Class("__unknown__".to_string()),
    }
}

/// Stage 8.7: 将 `MangleType` 序列化为可读文本 (供 requires/concept 体引用)。
///
/// 与 `type_key` 不同, 这里输出的是 C++ 风格的类型名 (含 `*`/`&`/`<>` 等标点),
/// 让 `evaluate_constraint` 内部表达式解析时能匹配源程序中的字面量。
pub fn mangle_type_to_text(ty: &MangleType) -> String {
    match ty {
        MangleType::Void => "void".into(),
        MangleType::Bool => "bool".into(),
        MangleType::Char => "char".into(),
        MangleType::Schar => "signed char".into(),
        MangleType::Uchar => "unsigned char".into(),
        MangleType::Short => "short".into(),
        MangleType::Ushort => "unsigned short".into(),
        MangleType::Int => "int".into(),
        MangleType::Uint => "unsigned int".into(),
        MangleType::Long => "long".into(),
        MangleType::Ulong => "unsigned long".into(),
        MangleType::LongLong => "long long".into(),
        MangleType::UlongLong => "unsigned long long".into(),
        MangleType::Float => "float".into(),
        MangleType::Double => "double".into(),
        MangleType::LongDouble => "long double".into(),
        MangleType::Pointer(t) => format!("{}*", mangle_type_to_text(t)),
        MangleType::Reference(t) => format!("{}&", mangle_type_to_text(t)),
        MangleType::Array(t, _) => format!("{}[]", mangle_type_to_text(t)),
        MangleType::Class(n) => n.clone(),
        MangleType::Template { name, args } => {
            let mut s = format!("{}<", name);
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&mangle_type_to_text(a));
            }
            s.push('>');
            s
        },
        MangleType::TplMember { name, args, member } => {
            let mut s = format!("{}<", name);
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&mangle_type_to_text(a));
            }
            s.push_str(">::");
            s.push_str(member);
            s
        },
        MangleType::NtExpr(op, operands) => {
            let mut s = op.key().to_string();
            s.push('<');
            for (i, a) in operands.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&mangle_type_to_text(a));
            }
            s.push('>');
            s
        },
        MangleType::VariadicPack(items) => {
            let mut s = String::new();
            for (i, a) in items.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&mangle_type_to_text(a));
            }
            s
        },
        MangleType::Function { ret, params } => {
            let mut s = format!("{}(", mangle_type_to_text(ret));
            for (i, a) in params.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&mangle_type_to_text(a));
            }
            s.push(')');
            s
        },
        MangleType::Ellipsis => "...".into(),
        MangleType::Auto => "auto".into(),
        MangleType::DecltypeAuto => "decltype(auto)".into(),
        MangleType::NonType(n) => n.to_string(),
        MangleType::TemplateParam(sym) => format!("tplparam_{}", sym.0),
    }
}
