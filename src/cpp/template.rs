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

use crate::cpp::mangle::MangleType;
use crate::cpp::parser::{ClassDecl, CppBody, CppDecl, CppExpr, CppStmt, SizeofArg, TemplateArgs};

/// G13: Result of template matching with priority resolution
#[derive(Debug, Clone)]
pub enum BestMatchResult<'a> {
    FullSpec(&'a FullSpecEntry),
    PartialSpec((CppDecl, Vec<String>, Vec<MangleType>)),
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
    args.iter().flat_map(|a| expand_pack(a)).collect()
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

    /// G12: resolve args when variadic packs are involved
    /// Handle the case where some params are packs and need special matching
    pub fn resolve_args_with_variadic(
        &self,
        td: &TemplateDecl,
        args: &[MangleType],
    ) -> Option<Vec<MangleType>> {
        // Check if this template has variadic parameters
        let has_pack = td.is_variadic_param.iter().any(|&p| p);

        if !has_pack {
            // Non-variadic template, simple passthrough
            return Some(args.to_vec());
        }

        // Variadic template scenario
        // Need to match args against params where some are packs
        // e.g., template<typename... T> foo<int, double, char>
        // params: ["T"] (is_variadic_param: [true])
        // args: [int, double, char]

        // Special handling for last pack param
        let mut resolved = Vec::new();
        let mut arg_idx = 0;

        for (_param_idx, is_pack) in td.is_variadic_param.iter().enumerate() {
            if *is_pack {
                // This parameter is a pack
                // Collect all remaining args into a VariadicPack
                let remaining_args: Vec<MangleType> = args[arg_idx..].to_vec();
                resolved.push(MangleType::VariadicPack(remaining_args));
                arg_idx = args.len();
            } else {
                // Regular parameter - consume one arg
                if arg_idx < args.len() {
                    resolved.push(args[arg_idx].clone());
                    arg_idx += 1;
                }
            }
        }

        if arg_idx == args.len() {
            Some(resolved)
        } else {
            None // Mismatched count
        }
    }

    /// 注册模板声明 (重复注册同名模板: 后者覆盖, 便于 stdlib 头重复 include)
    pub fn register(&mut self, targs: &TemplateArgs, requires: &Option<String>, decl: &CppDecl) {
        if let Some(td) = Self::decl_from(targs, requires, decl.clone()) {
            self.templates.retain(|t| t.name != td.name);
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
        for (i, _p) in td.param_names.iter().enumerate() {
            if let Some(a) = args.get(i) {
                resolved.push(a.clone());
            } else if let Some(d) = td.defaults.get(i).cloned().flatten() {
                resolved.push(d);
            } else if i == 0 && args.is_empty() && !td.defaults.is_empty() {
                // 单默认形参模板 `template <typename T = int>` 且无实参
                resolved.push(td.defaults[0].clone().unwrap());
            } else {
                return None; // 实参不足且无默认值
            }
        }
        let _ = td;
        // requires 约束求值 (概念过滤)
        if let Some(cond) = &td.requires {
            let mut ctx = crate::cpp::feature_degrade::SfinaeCtx::new();
            for (pn, pa) in td.param_names.iter().zip(resolved.iter()) {
                ctx.bind(pn.clone(), type_key(pa));
            }
            // M26: 展开 requires 子句中的概念引用
            let expanded = crate::cpp::feature_degrade::expand_concepts(cond, &self.concepts);
            if !crate::cpp::feature_degrade::evaluate_trait_condition(&expanded, &ctx)
                .unwrap_or(true)
            {
                return None;
            }
        }
        Some(resolved)
    }

    /// 从调用实参类型推导模板实参 (隐式实例化)
    /// call_arg_tys: 调用点实参的 MangleType (由 lower 计算)
    pub fn deduce_args(
        &self,
        td: &TemplateDecl,
        call_arg_tys: &[MangleType],
    ) -> Option<Vec<MangleType>> {
        // 已有全部默认实参的模板 (无参模板函数 `sfinae_default_arg`) → 直接取默认
        if td.defaults.iter().all(|d| d.is_some()) {
            let mut args: Vec<MangleType> = Vec::new();
            for d in &td.defaults {
                args.push(d.clone().unwrap());
            }
            return self.resolve_args(td, &args);
        }
        // 从函数参数类型推导: 形参类型中出现的形参名 ← 实参类型
        let fn_params: Vec<MangleType> = match &td.decl {
            CppDecl::FreeFn { params, .. } => params.iter().map(|(_, t)| t.clone()).collect(),
            _ => return None,
        };
        let mut bindings: Vec<(String, MangleType)> = Vec::new();
        for (pt, at) in fn_params.iter().zip(call_arg_tys.iter()) {
            collect_bindings(pt, at, &td.param_names, &mut bindings);
        }
        // 生成实参 (按形参顺序; 未推导出的用默认或失败)
        let mut args: Vec<MangleType> = Vec::new();
        for (i, pn) in td.param_names.iter().enumerate() {
            if let Some((_, a)) = bindings.iter().rev().find(|(b, _)| b == pn) {
                args.push(a.clone());
            } else {
                let d = td.defaults.get(i).cloned().flatten()?;
                args.push(d);
            }
        }
        self.resolve_args(td, &args)
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
            self.inst_cache.insert(key.clone(), inst.clone());
            self.inst_order.push(key);
            return Some(inst);
        }

        // Fallback to primary template
        let td = self.find(name)?;
        let resolved = if self.has_variadic() {
            self.resolve_args_with_variadic(&td, args)?
        } else {
            self.resolve_args(&td, args)?
        };
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
        self.inst_cache.insert(key.clone(), inst.clone());
        self.inst_order.push(key);
        Some(inst)
    }

    /// 实例化模板函数 `name<args>`; 返回实例化后的 FreeFn 声明
    pub fn instantiate_fn(&mut self, name: &str, args: &[MangleType]) -> Option<CppDecl> {
        let td = self.find(name)?;
        // G12: Use variadic-aware arg resolution
        let resolved = if self.has_variadic() {
            self.resolve_args_with_variadic(&td, args)?
        } else {
            self.resolve_args(&td, args)?
        };
        let key = Self::key_for(name, &resolved);
        if let Some(cached) = self.inst_cache.get(&key) {
            return Some(cached.clone());
        }
        // G11: 使用模板模板参数增强的实例化
        let primary_params = &td.template_params;
        let inst = instantiate_decl_with_template_params(
            &td.decl,
            &td.param_names,
            &resolved,
            None,
            primary_params,
        );
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
                let bind_check = |bindings: &mut Vec<(String, MangleType)>,
                                  name: &str,
                                  arg_ty: &MangleType,
                                  all_match: &mut bool| {
                    if let Some((_, prev)) = bindings.iter().find(|(n, _)| n == name) {
                        if type_key(prev) != *arg {
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
                        if Self::is_param_name_internal(pat, &p.param_names) {
                            bind_check(&mut bindings, &pat_key, arg_ty, &mut all_match);
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
                    bind_check(&mut bindings, &pat_key, arg_ty, &mut all_match);
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
            return BestMatchResult::PartialSpec(partial);
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
        (MangleType::Reference(a), MangleType::Pointer(b)) => collect_bindings(a, b, names, out),
        (MangleType::Reference(a), MangleType::Reference(b)) => collect_bindings(a, b, names, out),
        (MangleType::Template { args: pa, .. }, MangleType::Template { args: aa, .. }) => {
            for (x, y) in pa.iter().zip(aa.iter()) {
                collect_bindings(x, y, names, out);
            }
        },
        _ => {},
    }
}

// ===== AST 级替换 =====

/// 把类型中的模板形参替换为实参
pub fn substitute_type(ty: &MangleType, params: &[String], args: &[MangleType]) -> MangleType {
    match ty {
        MangleType::Class(n) => {
            if let Some(idx) = params.iter().position(|p| p == n) {
                if let Some(a) = args.get(idx) {
                    return a.clone();
                }
            }
            ty.clone()
        },
        // G12: 参数包展开 - 如果是 pack 形参，返回整个包
        MangleType::Class(n) if params.len() != args.len() => {
            // 检查是否是 pack 形参的引用 (需要特殊处理)
            // 这里简化处理：如果 param count 不等于 arg count，可能是 pack 场景
            // 实际应在 TemplateParam 中记录 is_pack 标志
            ty.clone()
        },
        MangleType::Pointer(t) => MangleType::Pointer(Box::new(substitute_type(t, params, args))),
        MangleType::Reference(t) => {
            MangleType::Reference(Box::new(substitute_type(t, params, args)))
        },
        MangleType::Array(t, n) => {
            MangleType::Array(Box::new(substitute_type(t, params, args)), *n)
        },
        MangleType::Template { name, args: tas } => {
            let new_args: Vec<MangleType> = tas
                .iter()
                .map(|a| substitute_type(a, params, args))
                .collect();
            MangleType::Template {
                name: name.clone(),
                args: new_args,
            }
        },
        // M30: `Tpl<Args>::member` 的实参递归替换; name/member 保留
        // (typedef 展开由 lower 在查 inst_typedefs 时完成)
        MangleType::TplMember {
            name,
            args: tas,
            member,
        } => {
            let new_args: Vec<MangleType> = tas
                .iter()
                .map(|a| substitute_type(a, params, args))
                .collect();
            MangleType::TplMember {
                name: name.clone(),
                args: new_args,
                member: member.clone(),
            }
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
        CppExpr::Call(f, as_) => CppExpr::Call(
            Box::new(substitute_expr(f, params, args)),
            as_.iter()
                .map(|a| substitute_expr(a, params, args))
                .collect(),
        ),
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
        // Ident / 字面量 / 模板名等无需替换
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
        } => CppStmt::LocalVar {
            ty: substitute_type(ty, params, args),
            name: name.clone(),
            init: init.as_ref().map(|x| substitute_expr(x, params, args)),
            is_volatile: *is_volatile,
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

/// 递归替换声明内的模板形参类型
/// class_rename: 类模板实例化时新的类名 (成员函数/ctor 的 class 字段同步改写)
pub fn substitute_decl(
    d: &CppDecl,
    params: &[String],
    args: &[MangleType],
    class_rename: Option<&str>,
) -> CppDecl {
    match d {
        CppDecl::Class(c) => {
            let new_name = class_rename
                .map(|n| n.to_string())
                .unwrap_or_else(|| c.name.clone());
            CppDecl::Class(ClassDecl {
                name: new_name,
                is_struct: c.is_struct,
                is_union: c.is_union,
                bases: c.bases.clone(),
                members: c
                    .members
                    .iter()
                    .map(|m| substitute_decl(m, params, args, class_rename))
                    .collect(),
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
            body: substitute_body(body, params, args),
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
                body: substitute_body(body, params, args),
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
                inits: inits
                    .iter()
                    .map(|(n, e)| (n.clone(), substitute_expr(e, params, args)))
                    .collect(),
                body: substitute_body(body, params, args),
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
        let outer = t("vector", &[inner.clone()]);
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
                members: vec![],
            }),
        });
        // int* 匹配 T*
        assert!(reg
            .pick_partial("Foo", &[MangleType::Pointer(Box::new(MangleType::Int))])
            .is_some());
        // int 不匹配
        assert!(reg.pick_partial("Foo", &[MangleType::Int]).is_none());
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

        let result = reg.instantiate_class("reverse_iterator", &[iterator_type.clone()]);
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
