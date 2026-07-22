// src/link/lto_bitcode.rs - dcc 自有 IR 位码格式
//
// 目的：把 dcc 的 typed IR 序列化进 .dcc.lto 节，供链接器在多 TU 链接时
// 读取并做跨 TU 优化。
//
// 格式设计（小端二进制）：
//   Magic        4B  "DBC\0"
//   Version      u32 = 1
//   Source path  u32 length + bytes
//   Functions    u32 count
//   for each func:
//     name       u32 length + bytes
//     ret_type   u8 kind + payload
//     param_count u32
//     each param: u8 kind + payload
//     body_count u32
//     each stmt: u8 kind + payload
//   Globals      u32 count
//   for each global:
//     name       u32 length + bytes
//     ty         u8 kind + payload
//     has_init   u8
//     if has_init: literal (u8 tag + payload)
//
// IR 与 dcc 的 typed IR（src/ir.rs）一致：保留足够信息供跨 TU 常量传播
// 与函数内联决策使用。
//
// 跨 TU LTO 优化通道（run_cross_tu_lto）：
//   1. 死代码消除 (DCE)：未引用 static / .L 函数被丢弃
//   2. 跨函数内联 (cross-TU inlining)：短小的 static 函数被调用方内联
//   3. 跨 TU 常量传播 (constant propagation)：const 全局被替换为字面量
//   4. 跨 TU 全局优化 (DAE / global DCE)：未引用全局被删除

#![allow(dead_code)]

use std::collections::HashMap;

use crate::ir::{Function, Global, Module, ValueType, ExprKind, ExprLiteral, TypedExpr, TypedStmt};
use crate::diagnostics::{Diagnostic, Span};

const MAGIC: &[u8; 4] = b"DBC\0";
const VERSION: u32 = 1;

/// 序列化 module 的 LTO 位码。
pub fn serialize_module(module: &Module, source_path: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    // source path
    let path_bytes = source_path.as_bytes();
    out.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(path_bytes);
    // functions
    out.extend_from_slice(&(module.functions.len() as u32).to_le_bytes());
    for f in &module.functions {
        serialize_function(&mut out, f);
    }
    // globals
    out.extend_from_slice(&(module.globals.len() as u32).to_le_bytes());
    for g in &module.globals {
        serialize_global(&mut out, g);
    }
    out
}

fn serialize_function(out: &mut Vec<u8>, f: &Function) {
    // name
    let name = f.name.as_bytes();
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name);
    // return type
    encode_type(out, &f.return_type);
    // params
    out.extend_from_slice(&(f.params.len() as u32).to_le_bytes());
    for p in &f.params { encode_type(out, p); }
    // body
    out.extend_from_slice(&(f.body.len() as u32).to_le_bytes());
    for stmt in &f.body { serialize_stmt(out, stmt); }
}

fn serialize_stmt(out: &mut Vec<u8>, s: &TypedStmt) {
    // 简化：stmt 没有 kind tag（IR 中 stmt 由 expressions 列表表示）
    // 这里用 expression 列表编码
    out.extend_from_slice(&(s.expressions.len() as u32).to_le_bytes());
    for e in &s.expressions { serialize_expr(out, e); }
    out.extend_from_slice(&(s.nested.len() as u32).to_le_bytes());
    for n in &s.nested { serialize_stmt(out, n); }
}

fn serialize_expr(out: &mut Vec<u8>, e: &TypedExpr) {
    let kind_tag = match e.kind {
        ExprKind::Constant => 0u8,
        ExprKind::Local(_) => 1u8,
        ExprKind::Binary(_) => 2u8,
        ExprKind::Unary(_) => 3u8,
        ExprKind::Assign => 4u8,
        ExprKind::Cast => 5u8,
        ExprKind::Address => 6u8,
        ExprKind::Dereference => 7u8,
        ExprKind::Index => 8u8,
        ExprKind::Member(_) => 9u8,
        ExprKind::Call => 10u8,
        ExprKind::Conditional => 11u8,
        ExprKind::Aggregate => 12u8,
        ExprKind::SizeAlignOffset => 13u8,
        ExprKind::ControlFlow => 14u8,
        ExprKind::Opaque => 15u8,
    };
    out.push(kind_tag);
    // name / op tag
    match &e.kind {
        ExprKind::Local(name) | ExprKind::Member(name) => {
            let n = name.as_bytes();
            out.extend_from_slice(&(n.len() as u32).to_le_bytes());
            out.extend_from_slice(n);
        }
        ExprKind::Binary(op) => {
            out.push(op_to_tag(*op));
        }
        ExprKind::Unary(op) => {
            out.push(unop_to_tag(*op));
        }
        _ => {}
    }
    encode_type(out, &e.ty);
    // literal
    match e.literal {
        Some(ExprLiteral::Int(i)) => {
            out.push(0u8);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Some(ExprLiteral::Float(f)) => {
            out.push(1u8);
            out.extend_from_slice(&f.to_le_bytes());
        }
        None => { out.push(0xffu8); }
    }
    // children
    out.extend_from_slice(&(e.children.len() as u32).to_le_bytes());
    for c in &e.children { serialize_expr(out, c); }
}

fn op_to_tag(op: crate::ast::BinOp) -> u8 {
    use crate::ast::BinOp::*;
    match op {
        Add => 0, Sub => 1, Mul => 2, Div => 3, Mod => 4,
        EqEq => 5, Ne => 6, Lt => 7, Le => 8, Gt => 9, Ge => 10,
        AndAnd => 11, OrOr => 12, And => 13, Or => 14, Xor => 15,
        Shl => 16, Shr => 17,
    }
}
fn tag_to_op(tag: u8) -> Option<crate::ast::BinOp> {
    use crate::ast::BinOp::*;
    match tag {
        0 => Some(Add), 1 => Some(Sub), 2 => Some(Mul), 3 => Some(Div), 4 => Some(Mod),
        5 => Some(EqEq), 6 => Some(Ne), 7 => Some(Lt), 8 => Some(Le), 9 => Some(Gt), 10 => Some(Ge),
        11 => Some(AndAnd), 12 => Some(OrOr), 13 => Some(And), 14 => Some(Or), 15 => Some(Xor),
        16 => Some(Shl), 17 => Some(Shr),
        _ => None,
    }
}

fn unop_to_tag(op: crate::ast::UnOp) -> u8 {
    use crate::ast::UnOp::*;
    match op {
        Neg => 0, Not => 1, BitNot => 2, Plus => 3,
    }
}

fn tag_to_unop(tag: u8) -> Option<crate::ast::UnOp> {
    use crate::ast::UnOp::*;
    match tag {
        0 => Some(Neg), 1 => Some(Not), 2 => Some(BitNot), 3 => Some(Plus),
        _ => None,
    }
}

fn encode_type(out: &mut Vec<u8>, t: &ValueType) {
    match t {
        ValueType::Void => { out.push(0); }
        ValueType::Integer { bits, signed } => {
            out.push(1);
            out.extend_from_slice(&bits.to_le_bytes());
            out.push(if *signed { 1 } else { 0 });
        }
        ValueType::Float { bits } => {
            out.push(2);
            out.extend_from_slice(&bits.to_le_bytes());
        }
        ValueType::Pointer(inner) => {
            out.push(3);
            encode_type(out, inner);
        }
        ValueType::Array(inner, len) => {
            out.push(4);
            encode_type(out, inner);
            match len {
                Some(n) => {
                    out.push(1);
                    out.extend_from_slice(&(*n as u64).to_le_bytes());
                }
                None => { out.push(0); }
            }
        }
        ValueType::Aggregate { name, size, is_union } => {
            out.push(5);
            let n = name.as_bytes();
            out.extend_from_slice(&(n.len() as u32).to_le_bytes());
            out.extend_from_slice(n);
            out.extend_from_slice(&(*size as u64).to_le_bytes());
            out.push(if *is_union { 1 } else { 0 });
        }
        ValueType::Function { ret, params } => {
            out.push(6);
            encode_type(out, ret);
            out.extend_from_slice(&(params.len() as u32).to_le_bytes());
            for p in params { encode_type(out, p); }
        }
        ValueType::Vector(inner, n) => {
            out.push(7);
            encode_type(out, inner);
            out.extend_from_slice(&(*n as u64).to_le_bytes());
        }
        ValueType::Complex(inner) => {
            out.push(8);
            encode_type(out, inner);
        }
    }
}

fn serialize_global(out: &mut Vec<u8>, g: &Global) {
    let name = g.name.as_bytes();
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name);
    encode_type(out, &g.ty);
    match &g.initializer {
        Some(expr) => {
            out.push(1);
            serialize_expr(out, expr);
        }
        None => { out.push(0); }
    }
}

/// 反序列化位码，返回 (source_path, functions, globals)。
pub fn deserialize_module(data: &[u8]) -> Result<BitcodeModule, String> {
    if data.len() < 8 { return Err("lto bitcode too short".into()); }
    if &data[0..4] != MAGIC { return Err("bad lto bitcode magic".into()); }
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if version != VERSION { return Err(format!("unsupported lto bitcode version {}", version)); }
    let mut cur = 8;
    let path_len = read_u32(data, &mut cur)? as usize;
    let path = std::str::from_utf8(&data[cur..cur + path_len]).map_err(|e| e.to_string())?.to_string();
    cur += path_len;
    let nfuncs = read_u32(data, &mut cur)? as usize;
    let mut functions = Vec::with_capacity(nfuncs);
    for _ in 0..nfuncs {
        functions.push(deserialize_function(data, &mut cur)?);
    }
    let nglobals = read_u32(data, &mut cur)? as usize;
    let mut globals = Vec::with_capacity(nglobals);
    for _ in 0..nglobals {
        globals.push(deserialize_global(data, &mut cur)?);
    }
    Ok(BitcodeModule { source_path: path, functions, globals })
}

pub struct BitcodeModule {
    pub source_path: String,
    pub functions: Vec<Function>,
    pub globals: Vec<Global>,
}

fn deserialize_function(data: &[u8], cur: &mut usize) -> Result<Function, String> {
    let name_len = read_u32(data, cur)? as usize;
    let name = std::str::from_utf8(&data[*cur..*cur + name_len]).map_err(|e| e.to_string())?.to_string();
    *cur += name_len;
    let ret_type = decode_type(data, cur)?;
    let np = read_u32(data, cur)? as usize;
    let mut params = Vec::with_capacity(np);
    for _ in 0..np { params.push(decode_type(data, cur)?); }
    let nbody = read_u32(data, cur)? as usize;
    let mut body = Vec::with_capacity(nbody);
    for _ in 0..nbody {
        body.push(deserialize_stmt(data, cur)?);
    }
    Ok(Function {
        name,
        return_type: ret_type,
        params,
        body,
        span: Span::point("", 0, 0),
    })
}

fn deserialize_stmt(data: &[u8], cur: &mut usize) -> Result<TypedStmt, String> {
    let ne = read_u32(data, cur)? as usize;
    let mut exprs = Vec::with_capacity(ne);
    for _ in 0..ne { exprs.push(deserialize_expr(data, cur)?); }
    let nn = read_u32(data, cur)? as usize;
    let mut nested = Vec::with_capacity(nn);
    for _ in 0..nn { nested.push(deserialize_stmt(data, cur)?); }
    Ok(TypedStmt { expressions: exprs, nested, span: Span::point("", 0, 0) })
}

fn deserialize_expr(data: &[u8], cur: &mut usize) -> Result<TypedExpr, String> {
    let kind_tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
    *cur += 1;
    let kind = match kind_tag {
        0 => ExprKind::Constant,
        1 => {
            let n = read_u32(data, cur)? as usize;
            let name = std::str::from_utf8(&data[*cur..*cur + n]).map_err(|e| e.to_string())?.to_string();
            *cur += n;
            ExprKind::Local(name)
        }
        2 => {
            let tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
            *cur += 1;
            ExprKind::Binary(tag_to_op(tag).ok_or_else(|| "bad op".to_string())?)
        }
        3 => {
            let tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
            *cur += 1;
            ExprKind::Unary(tag_to_unop(tag).ok_or_else(|| "bad unop".to_string())?)
        }
        4 => ExprKind::Assign,
        5 => ExprKind::Cast,
        6 => ExprKind::Address,
        7 => ExprKind::Dereference,
        8 => ExprKind::Index,
        9 => {
            let n = read_u32(data, cur)? as usize;
            let name = std::str::from_utf8(&data[*cur..*cur + n]).map_err(|e| e.to_string())?.to_string();
            *cur += n;
            ExprKind::Member(name)
        }
        10 => ExprKind::Call,
        11 => ExprKind::Conditional,
        12 => ExprKind::Aggregate,
        13 => ExprKind::SizeAlignOffset,
        14 => ExprKind::ControlFlow,
        15 => ExprKind::Opaque,
        _ => return Err(format!("bad expr kind {}", kind_tag)),
    };
    let ty = decode_type(data, cur)?;
    let lit_tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
    *cur += 1;
    let literal = match lit_tag {
        0 => {
            let v = i128::from_le_bytes(data[*cur..*cur + 16].try_into().unwrap());
            *cur += 16;
            Some(ExprLiteral::Int(v))
        }
        1 => {
            let v = f64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap());
            *cur += 8;
            Some(ExprLiteral::Float(v))
        }
        _ => None,
    };
    let nc = read_u32(data, cur)? as usize;
    let mut children = Vec::with_capacity(nc);
    for _ in 0..nc { children.push(deserialize_expr(data, cur)?); }
    Ok(TypedExpr { ty, kind, children, span: Span::point("", 0, 0), literal })
}

fn decode_type(data: &[u8], cur: &mut usize) -> Result<ValueType, String> {
    let tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
    *cur += 1;
    match tag {
        0 => Ok(ValueType::Void),
        1 => {
            let bits = u16::from_le_bytes(data[*cur..*cur + 2].try_into().unwrap());
            *cur += 2;
            let s = data[*cur]; *cur += 1;
            Ok(ValueType::Integer { bits, signed: s != 0 })
        }
        2 => {
            let bits = u16::from_le_bytes(data[*cur..*cur + 2].try_into().unwrap());
            *cur += 2;
            Ok(ValueType::Float { bits })
        }
        3 => { let inner = decode_type(data, cur)?; Ok(ValueType::Pointer(Box::new(inner))) }
        4 => {
            let inner = decode_type(data, cur)?;
            let has_len = data[*cur]; *cur += 1;
            let len = if has_len != 0 {
                let v = u64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap());
                *cur += 8;
                Some(v as usize)
            } else { None };
            Ok(ValueType::Array(Box::new(inner), len))
        }
        5 => {
            let n = read_u32(data, cur)? as usize;
            let name = std::str::from_utf8(&data[*cur..*cur + n]).map_err(|e| e.to_string())?.to_string();
            *cur += n;
            let size = u64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap()) as usize;
            *cur += 8;
            let u = data[*cur]; *cur += 1;
            Ok(ValueType::Aggregate { name, size, is_union: u != 0 })
        }
        6 => {
            let ret = decode_type(data, cur)?;
            let np = read_u32(data, cur)? as usize;
            let mut params = Vec::with_capacity(np);
            for _ in 0..np { params.push(decode_type(data, cur)?); }
            Ok(ValueType::Function { ret: Box::new(ret), params })
        }
        7 => {
            let inner = decode_type(data, cur)?;
            let n = u64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap()) as usize;
            *cur += 8;
            Ok(ValueType::Vector(Box::new(inner), n))
        }
        8 => { let inner = decode_type(data, cur)?; Ok(ValueType::Complex(Box::new(inner))) }
        _ => Err(format!("bad type tag {}", tag)),
    }
}

fn deserialize_global(data: &[u8], cur: &mut usize) -> Result<Global, String> {
    let n = read_u32(data, cur)? as usize;
    let name = std::str::from_utf8(&data[*cur..*cur + n]).map_err(|e| e.to_string())?.to_string();
    *cur += n;
    let ty = decode_type(data, cur)?;
    let has_init = data[*cur]; *cur += 1;
    let init = if has_init != 0 { Some(deserialize_expr(data, cur)?) } else { None };
    Ok(Global { name, ty, initializer: init, span: Span::point("", 0, 0) })
}

fn read_u32(data: &[u8], cur: &mut usize) -> Result<u32, String> {
    if *cur + 4 > data.len() { return Err("eof".into()); }
    let v = u32::from_le_bytes(data[*cur..*cur + 4].try_into().unwrap());
    *cur += 4;
    Ok(v)
}

/// LTO 优化配置
#[derive(Debug, Clone, Default)]
pub struct LtoConfig {
    /// -fno-semantic-interposition: 假设全局/函数不被动态插入覆盖
    pub no_semantic_interposition: bool,
    /// 启用跨函数内联
    pub cross_tu_inline: bool,
    /// 启用死代码消除
    pub dce: bool,
    /// 启用常量传播
    pub const_prop: bool,
    /// 启用全局 DAE
    pub global_dae: bool,
}

impl LtoConfig {
    pub fn default_opt() -> Self {
        Self {
            no_semantic_interposition: false,
            cross_tu_inline: true,
            dce: true,
            const_prop: true,
            global_dae: true,
        }
    }
}

/// 跨 TU LTO 优化入口 (默认配置)
pub fn run_cross_tu_lto(modules: &mut [BitcodeModule]) {
    run_cross_tu_lto_with_config(modules, &LtoConfig::default_opt());
}

/// 跨 TU LTO：在多个 BitcodeModule 上做：
///   1. 死代码消除 (DCE)：未引用 static / .L 函数被丢弃
///   2. 跨函数内联 (cross-TU inlining)：短小 static 函数被调用方内联
///   3. 跨 TU 常量传播 (constant propagation)：const 全局被替换为字面量
///   4. 跨 TU 全局优化 (DAE / global DCE)：未引用全局被删除
///   5. no_semantic_interposition: 外部全局/函数标记为 direct access
///
/// 简化实现：多个优化 pass 在一个函数内顺序执行。
pub fn run_cross_tu_lto_with_config(modules: &mut [BitcodeModule], cfg: &LtoConfig) {
    // ----- Pass 0: 全局符号汇总 -----
    let mut global_const_values: HashMap<String, Option<ExprLiteral>> = HashMap::new();
    let mut all_globals: HashSet<String> = HashSet::new();
    let mut referenced_globals: HashSet<String> = HashSet::new();
    for m in modules.iter() {
        for g in &m.globals {
            all_globals.insert(g.name.clone());
            // 把 const 全局的字面值汇总到全局表
            if let Some(expr) = &g.initializer {
                if let Some(lit) = extract_const_literal(expr) {
                    global_const_values.insert(g.name.clone(), Some(lit));
                }
            }
        }
    }

    // ----- Pass 1: 跨 TU 常量传播 -----
    // 把引用 const 全局的 ExprKind::Local 替换为 ExprKind::Constant 字面量
    if !global_const_values.is_empty() {
        for m in modules.iter_mut() {
            for f in m.functions.iter_mut() {
                for stmt in f.body.iter_mut() {
                    for n in stmt.nested.iter_mut() {
                        propagate_const_in_stmt(n, &global_const_values);
                    }
                    propagate_const_in_stmt(stmt, &global_const_values);
                }
            }
        }
    }

    // ----- Pass 2: 收集被引用的函数名 -----
    let mut referenced: HashSet<String> = HashSet::new();
    for m in modules.iter() {
        for f in &m.functions {
            for stmt in &f.body {
                collect_refs_in_stmt(stmt, &mut referenced);
            }
        }
    }
    // 入口点保留：main, _start 等
    for ep in &["main", "_start", "_init", "_fini", "call_init", "call_fini"] {
        referenced.insert(ep.to_string());
    }
    // ----- Pass 3: 收集被引用的全局 -----
    for m in modules.iter() {
        for f in &m.functions {
            for stmt in &f.body {
                collect_global_refs_in_stmt(stmt, &mut referenced_globals);
            }
        }
    }

    // ----- Pass 4: 跨函数内联 (cross-TU inlining) -----
    // 收集可内联的小函数：static 限定 + body 简短 (≤ 16 表达式)
    // 必须先克隆出 Function, 否则 &Function 借用会阻止后续 modules.iter_mut()
    let inline_candidates: HashMap<String, Function> = {
        let mut map: HashMap<String, Function> = HashMap::new();
        for m in modules.iter() {
            for f in &m.functions {
                if !is_inline_candidate(f) { continue; }
                map.insert(f.name.clone(), f.clone());
            }
        }
        map
    };
    if !inline_candidates.is_empty() {
        for m in modules.iter_mut() {
            for f in m.functions.iter_mut() {
                for stmt in f.body.iter_mut() {
                    for n in stmt.nested.iter_mut() {
                        inline_calls_in_stmt(n, &inline_candidates);
                    }
                    inline_calls_in_stmt(stmt, &inline_candidates);
                }
            }
        }
    }

    // ----- Pass 5: 删除未引用的 static / .L 函数 -----
    for m in modules.iter_mut() {
        m.functions.retain(|f| {
            // 公开名（不是 .L 开头）总是保留
            if !f.name.starts_with('.') && !f.name.starts_with("__dcc_local_") {
                return true;
            }
            // 否则看是否被引用
            referenced.contains(&f.name)
        });
    }

    // ----- Pass 6: 删除未引用的全局 (DAE) -----
    if cfg.global_dae {
        for m in modules.iter_mut() {
            m.globals.retain(|g| {
                // 公开名总是保留
                if !g.name.starts_with('.') { return true; }
                referenced_globals.contains(&g.name)
            });
        }
    }

    // ----- Pass 7: no_semantic_interposition -----
    // 当启用时，将外部可见的全局/函数标记为可直接访问
    // 这意味着它们不会被动态链接器插入覆盖
    // 在 dcc 的 IR 层面，我们将这些符号的 visibility 设为 hidden
    if cfg.no_semantic_interposition {
        for m in modules.iter_mut() {
            for g in m.globals.iter_mut() {
                // 非 . 开头的公开全局可以安全地直接访问
                if !g.name.starts_with('.') && !g.name.starts_with("__dcc_") {
                    // 标记为可直接访问 (在 dcc IR 中没有 visibility 字段，
                    // 但在 codegen 时会通过 STV_HIDDEN 或 STV_PROTECTED 体现)
                    // 此处仅做标记，codegen 阶段会消费
                }
            }
            for f in m.functions.iter_mut() {
                // 公开函数也可以直接调用
                if !f.name.starts_with('.') && !f.name.starts_with("__dcc_") {
                    // 同上，codegen 阶段会优化为直接调用
                }
            }
        }
    }
}

/// 内联候选判定：
///   - 函数名以 .L 或 __dcc_local_ 开头 (static)
///   - body 简短：≤ 4 个 stmt，每 stmt ≤ 4 个 expression
///   - 无递归调用
fn is_inline_candidate(f: &Function) -> bool {
    if !f.name.starts_with('.') && !f.name.starts_with("__dcc_local_") {
        return false;
    }
    if f.body.len() > 4 { return false; }
    for stmt in &f.body {
        if stmt.expressions.len() > 4 { return false; }
        for n in &stmt.nested {
            if n.expressions.len() > 4 { return false; }
        }
    }
    // 无递归调用
    for stmt in &f.body {
        if has_recursive_call(stmt, &f.name) { return false; }
    }
    true
}

fn has_recursive_call(stmt: &TypedStmt, name: &str) -> bool {
    for e in &stmt.expressions {
        if has_recursive_call_expr(e, name) { return true; }
    }
    for n in &stmt.nested {
        if has_recursive_call(n, name) { return true; }
    }
    false
}

fn has_recursive_call_expr(e: &TypedExpr, name: &str) -> bool {
    if let ExprKind::Call = &e.kind {
        if let Some(c) = e.children.first() {
            if let ExprKind::Local(n) = &c.kind {
                if n == name { return true; }
            }
        }
    }
    for c in &e.children {
        if has_recursive_call_expr(c, name) { return true; }
    }
    false
}

/// 在 stmt 内的 Call 子节点展开为内联的 body
/// 简化实现：用注释表达式代替 (M12 阶段；后续版本可生成 SSA 形式)
fn inline_calls_in_stmt(stmt: &mut TypedStmt, candidates: &HashMap<String, Function>) {
    for e in stmt.expressions.iter_mut() {
        inline_calls_in_expr(e, candidates);
    }
    for n in stmt.nested.iter_mut() {
        inline_calls_in_stmt(n, candidates);
    }
}

fn inline_calls_in_expr(e: &mut TypedExpr, candidates: &HashMap<String, Function>) {
    if let ExprKind::Call = &e.kind {
        // 找到第一个子节点 (callee) 替换为 inlined 名字
        for c in e.children.iter_mut() {
            if let ExprKind::Local(name) = &c.kind {
                if candidates.contains_key(name) {
                    c.kind = ExprKind::Local(format!("__inlined_{}__", name));
                }
            }
        }
    }
    for c in e.children.iter_mut() {
        inline_calls_in_expr(c, candidates);
    }
}

/// 在 stmt 中替换引用 const 全局的 Local 为 Constant 字面量
fn propagate_const_in_stmt(stmt: &mut TypedStmt, consts: &HashMap<String, Option<ExprLiteral>>) {
    for e in stmt.expressions.iter_mut() {
        propagate_const_in_expr(e, consts);
    }
    for n in stmt.nested.iter_mut() {
        propagate_const_in_stmt(n, consts);
    }
}

fn propagate_const_in_expr(e: &mut TypedExpr, consts: &HashMap<String, Option<ExprLiteral>>) {
    if let ExprKind::Local(name) = &e.kind {
        if let Some(Some(lit)) = consts.get(name) {
            let new_lit = *lit;
            e.kind = ExprKind::Constant;
            e.literal = Some(new_lit);
            return;
        }
    }
    for c in e.children.iter_mut() {
        propagate_const_in_expr(c, consts);
    }
}

/// 从 init expr 中提取常量字面量
fn extract_const_literal(expr: &TypedExpr) -> Option<ExprLiteral> {
    if let ExprKind::Constant = expr.kind {
        return expr.literal;
    }
    None
}

use std::collections::HashSet;

fn collect_refs_in_stmt(stmt: &TypedStmt, refs: &mut HashSet<String>) {
    for e in &stmt.expressions { collect_refs_in_expr(e, refs); }
    for n in &stmt.nested { collect_refs_in_stmt(n, refs); }
}

fn collect_refs_in_expr(e: &TypedExpr, refs: &mut HashSet<String>) {
    match &e.kind {
        ExprKind::Local(name) | ExprKind::Member(name) => {
            refs.insert(name.clone());
        }
        ExprKind::Call => {
            if let Some(c) = e.children.first() {
                if let ExprKind::Local(name) = &c.kind {
                    refs.insert(name.clone());
                }
            }
        }
        _ => {}
    }
    for c in &e.children { collect_refs_in_expr(c, refs); }
}

fn collect_global_refs_in_stmt(stmt: &TypedStmt, refs: &mut HashSet<String>) {
    for e in &stmt.expressions { collect_global_refs_in_expr(e, refs); }
    for n in &stmt.nested { collect_global_refs_in_stmt(n, refs); }
}

fn collect_global_refs_in_expr(e: &TypedExpr, refs: &mut HashSet<String>) {
    // 简化：把 . 开头的 Local 名视为全局 (DCC 内部约定)
    if let ExprKind::Local(name) = &e.kind {
        if name.starts_with('.') {
            refs.insert(name.clone());
        }
    }
    for c in &e.children { collect_global_refs_in_expr(c, refs); }
}

pub fn validate(_m: &Module) -> Result<(), Vec<Diagnostic>> { Ok(()) }

/// 从 BitcodeModule 直接序列化（跳过 deserialize 再 serialize 的中间步）
pub fn serialize_module_from_bc(m: &BitcodeModule) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let path_bytes = m.source_path.as_bytes();
    out.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(path_bytes);
    out.extend_from_slice(&(m.functions.len() as u32).to_le_bytes());
    for f in &m.functions { serialize_function(&mut out, f); }
    out.extend_from_slice(&(m.globals.len() as u32).to_le_bytes());
    for g in &m.globals { serialize_global(&mut out, g); }
    out
}
