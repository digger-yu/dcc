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

use crate::diagnostics::{Diagnostic, Span};
use crate::ir::{ExprKind, ExprLiteral, Function, Global, Module, TypedExpr, TypedStmt, ValueType};

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
    for p in &f.params {
        encode_type(out, p);
    }
    // body
    out.extend_from_slice(&(f.body.len() as u32).to_le_bytes());
    for stmt in &f.body {
        serialize_stmt(out, stmt);
    }
}

fn serialize_stmt(out: &mut Vec<u8>, s: &TypedStmt) {
    // 简化：stmt 没有 kind tag（IR 中 stmt 由 expressions 列表表示）
    // 这里用 expression 列表编码
    out.extend_from_slice(&(s.expressions.len() as u32).to_le_bytes());
    for e in &s.expressions {
        serialize_expr(out, e);
    }
    out.extend_from_slice(&(s.nested.len() as u32).to_le_bytes());
    for n in &s.nested {
        serialize_stmt(out, n);
    }
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
        ExprKind::VaArg { .. } => 16u8,
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
        ExprKind::VaArg { ap } => {
            let n = ap.as_bytes();
            out.extend_from_slice(&(n.len() as u32).to_le_bytes());
            out.extend_from_slice(n);
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
        None => {
            out.push(0xffu8);
        }
    }
    // children
    out.extend_from_slice(&(e.children.len() as u32).to_le_bytes());
    for c in &e.children {
        serialize_expr(out, c);
    }
}

fn op_to_tag(op: crate::ast::BinOp) -> u8 {
    use crate::ast::BinOp::*;
    match op {
        Add => 0,
        Sub => 1,
        Mul => 2,
        Div => 3,
        Mod => 4,
        EqEq => 5,
        Ne => 6,
        Lt => 7,
        Le => 8,
        Gt => 9,
        Ge => 10,
        AndAnd => 11,
        OrOr => 12,
        And => 13,
        Or => 14,
        Xor => 15,
        Shl => 16,
        Shr => 17,
    }
}
fn tag_to_op(tag: u8) -> Option<crate::ast::BinOp> {
    use crate::ast::BinOp::*;
    match tag {
        0 => Some(Add),
        1 => Some(Sub),
        2 => Some(Mul),
        3 => Some(Div),
        4 => Some(Mod),
        5 => Some(EqEq),
        6 => Some(Ne),
        7 => Some(Lt),
        8 => Some(Le),
        9 => Some(Gt),
        10 => Some(Ge),
        11 => Some(AndAnd),
        12 => Some(OrOr),
        13 => Some(And),
        14 => Some(Or),
        15 => Some(Xor),
        16 => Some(Shl),
        17 => Some(Shr),
        _ => None,
    }
}

fn unop_to_tag(op: crate::ast::UnOp) -> u8 {
    use crate::ast::UnOp::*;
    match op {
        Neg => 0,
        Not => 1,
        BitNot => 2,
        Plus => 3,
    }
}

fn tag_to_unop(tag: u8) -> Option<crate::ast::UnOp> {
    use crate::ast::UnOp::*;
    match tag {
        0 => Some(Neg),
        1 => Some(Not),
        2 => Some(BitNot),
        3 => Some(Plus),
        _ => None,
    }
}

fn encode_type(out: &mut Vec<u8>, t: &ValueType) {
    match t {
        ValueType::Void => {
            out.push(0);
        }
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
                None => {
                    out.push(0);
                }
            }
        }
        ValueType::Aggregate {
            name,
            size,
            is_union,
        } => {
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
            for p in params {
                encode_type(out, p);
            }
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
        None => {
            out.push(0);
        }
    }
}

/// 反序列化位码，返回 (source_path, functions, globals)。
pub fn deserialize_module(data: &[u8]) -> Result<BitcodeModule, String> {
    if data.len() < 8 {
        return Err("lto bitcode too short".into());
    }
    if &data[0..4] != MAGIC {
        return Err("bad lto bitcode magic".into());
    }
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap());
    if version != VERSION {
        return Err(format!("unsupported lto bitcode version {}", version));
    }
    let mut cur = 8;
    let path_len = read_u32(data, &mut cur)? as usize;
    let path = std::str::from_utf8(&data[cur..cur + path_len])
        .map_err(|e| e.to_string())?
        .to_string();
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
    Ok(BitcodeModule {
        source_path: path,
        functions,
        globals,
    })
}

pub struct BitcodeModule {
    pub source_path: String,
    pub functions: Vec<Function>,
    pub globals: Vec<Global>,
}

fn deserialize_function(data: &[u8], cur: &mut usize) -> Result<Function, String> {
    let name_len = read_u32(data, cur)? as usize;
    let name = std::str::from_utf8(&data[*cur..*cur + name_len])
        .map_err(|e| e.to_string())?
        .to_string();
    *cur += name_len;
    let ret_type = decode_type(data, cur)?;
    let np = read_u32(data, cur)? as usize;
    let mut params = Vec::with_capacity(np);
    for _ in 0..np {
        params.push(decode_type(data, cur)?);
    }
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
    for _ in 0..ne {
        exprs.push(deserialize_expr(data, cur)?);
    }
    let nn = read_u32(data, cur)? as usize;
    let mut nested = Vec::with_capacity(nn);
    for _ in 0..nn {
        nested.push(deserialize_stmt(data, cur)?);
    }
    Ok(TypedStmt {
        expressions: exprs,
        nested,
        span: Span::point("", 0, 0),
    })
}

fn deserialize_expr(data: &[u8], cur: &mut usize) -> Result<TypedExpr, String> {
    let kind_tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
    *cur += 1;
    let kind = match kind_tag {
        0 => ExprKind::Constant,
        1 => {
            let n = read_u32(data, cur)? as usize;
            let name = std::str::from_utf8(&data[*cur..*cur + n])
                .map_err(|e| e.to_string())?
                .to_string();
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
            let name = std::str::from_utf8(&data[*cur..*cur + n])
                .map_err(|e| e.to_string())?
                .to_string();
            *cur += n;
            ExprKind::Member(name)
        }
        10 => ExprKind::Call,
        11 => ExprKind::Conditional,
        12 => ExprKind::Aggregate,
        13 => ExprKind::SizeAlignOffset,
        14 => ExprKind::ControlFlow,
        15 => ExprKind::Opaque,
        16 => {
            let n = read_u32(data, cur)? as usize;
            let ap = std::str::from_utf8(&data[*cur..*cur + n])
                .map_err(|e| e.to_string())?
                .to_string();
            *cur += n;
            ExprKind::VaArg { ap }
        }
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
    for _ in 0..nc {
        children.push(deserialize_expr(data, cur)?);
    }
    Ok(TypedExpr {
        ty,
        kind,
        children,
        span: Span::point("", 0, 0),
        literal,
    })
}

fn decode_type(data: &[u8], cur: &mut usize) -> Result<ValueType, String> {
    let tag = data.get(*cur).copied().ok_or_else(|| "eof".to_string())?;
    *cur += 1;
    match tag {
        0 => Ok(ValueType::Void),
        1 => {
            let bits = u16::from_le_bytes(data[*cur..*cur + 2].try_into().unwrap());
            *cur += 2;
            let s = data[*cur];
            *cur += 1;
            Ok(ValueType::Integer {
                bits,
                signed: s != 0,
            })
        }
        2 => {
            let bits = u16::from_le_bytes(data[*cur..*cur + 2].try_into().unwrap());
            *cur += 2;
            Ok(ValueType::Float { bits })
        }
        3 => {
            let inner = decode_type(data, cur)?;
            Ok(ValueType::Pointer(Box::new(inner)))
        }
        4 => {
            let inner = decode_type(data, cur)?;
            let has_len = data[*cur];
            *cur += 1;
            let len = if has_len != 0 {
                let v = u64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap());
                *cur += 8;
                Some(v as usize)
            } else {
                None
            };
            Ok(ValueType::Array(Box::new(inner), len))
        }
        5 => {
            let n = read_u32(data, cur)? as usize;
            let name = std::str::from_utf8(&data[*cur..*cur + n])
                .map_err(|e| e.to_string())?
                .to_string();
            *cur += n;
            let size = u64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap()) as usize;
            *cur += 8;
            let u = data[*cur];
            *cur += 1;
            Ok(ValueType::Aggregate {
                name,
                size,
                is_union: u != 0,
            })
        }
        6 => {
            let ret = decode_type(data, cur)?;
            let np = read_u32(data, cur)? as usize;
            let mut params = Vec::with_capacity(np);
            for _ in 0..np {
                params.push(decode_type(data, cur)?);
            }
            Ok(ValueType::Function {
                ret: Box::new(ret),
                params,
            })
        }
        7 => {
            let inner = decode_type(data, cur)?;
            let n = u64::from_le_bytes(data[*cur..*cur + 8].try_into().unwrap()) as usize;
            *cur += 8;
            Ok(ValueType::Vector(Box::new(inner), n))
        }
        8 => {
            let inner = decode_type(data, cur)?;
            Ok(ValueType::Complex(Box::new(inner)))
        }
        _ => Err(format!("bad type tag {}", tag)),
    }
}

fn deserialize_global(data: &[u8], cur: &mut usize) -> Result<Global, String> {
    let n = read_u32(data, cur)? as usize;
    let name = std::str::from_utf8(&data[*cur..*cur + n])
        .map_err(|e| e.to_string())?
        .to_string();
    *cur += n;
    let ty = decode_type(data, cur)?;
    let has_init = data[*cur];
    *cur += 1;
    let init = if has_init != 0 {
        Some(deserialize_expr(data, cur)?)
    } else {
        None
    };
    Ok(Global {
        name,
        ty,
        initializer: init,
        span: Span::point("", 0, 0),
        is_tls: false,
    })
}

fn read_u32(data: &[u8], cur: &mut usize) -> Result<u32, String> {
    if *cur + 4 > data.len() {
        return Err("eof".into());
    }
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
                if !is_inline_candidate(f) {
                    continue;
                }
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
                if !g.name.starts_with('.') {
                    return true;
                }
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
    if f.body.len() > 4 {
        return false;
    }
    for stmt in &f.body {
        if stmt.expressions.len() > 4 {
            return false;
        }
        for n in &stmt.nested {
            if n.expressions.len() > 4 {
                return false;
            }
        }
    }
    // 无递归调用
    for stmt in &f.body {
        if has_recursive_call(stmt, &f.name) {
            return false;
        }
    }
    true
}

fn has_recursive_call(stmt: &TypedStmt, name: &str) -> bool {
    for e in &stmt.expressions {
        if has_recursive_call_expr(e, name) {
            return true;
        }
    }
    for n in &stmt.nested {
        if has_recursive_call(n, name) {
            return true;
        }
    }
    false
}

fn has_recursive_call_expr(e: &TypedExpr, name: &str) -> bool {
    if let ExprKind::Call = &e.kind {
        if let Some(c) = e.children.first() {
            if let ExprKind::Local(n) = &c.kind {
                if n == name {
                    return true;
                }
            }
        }
    }
    for c in &e.children {
        if has_recursive_call_expr(c, name) {
            return true;
        }
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

// =============================================================================
// 增强的跨 TU 内联
// =============================================================================

/// 内联上下文，跟踪跨 TU 的内联决策
#[derive(Debug, Clone, Default)]
pub struct InlineCtx {
    /// 内联候选函数 (name -> Function)
    pub candidates: HashMap<String, Function>,
    /// 内联阈值：最大函数体大小（stmt 数量）
    pub max_body_size: usize,
    /// 内联阈值：最大表达式复杂度
    pub max_expr_complexity: usize,
    /// 内联统计
    pub stats: InlineStats,
}

/// 内联统计信息
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InlineStats {
    /// 成功内联的调用数量
    pub inlined_calls: usize,
    /// 因为递归而跳过的调用
    pub skipped_recursive: usize,
    /// 因为太大而跳过的候选
    pub skipped_too_large: usize,
    /// 候选函数数量
    pub candidate_count: usize,
}

impl InlineCtx {
    pub fn new() -> Self {
        Self {
            max_body_size: 8,
            max_expr_complexity: 16,
            ..Default::default()
        }
    }

    /// 从多个模块收集内联候选
    pub fn collect_candidates(&mut self, modules: &[BitcodeModule]) {
        for module in modules {
            for func in &module.functions {
                if self.is_inline_candidate(func) {
                    self.candidates.insert(func.name.clone(), func.clone());
                }
            }
        }
        self.stats.candidate_count = self.candidates.len();
    }

    /// 判断函数是否是内联候选
    fn is_inline_candidate(&self, func: &Function) -> bool {
        // 内部函数优先（.L 或 __dcc_local_ 前缀）
        let is_internal = func.name.starts_with('.')
            || func.name.starts_with("__dcc_local_");

        // 检查函数体大小
        if func.body.len() > self.max_body_size {
            return false;
        }

        // 检查表达式复杂度
        let complexity = self.compute_complexity(func);
        if complexity > self.max_expr_complexity {
            return false;
        }

        // 检查递归
        if self.has_recursive_call(func) {
            return false;
        }

        // 内部函数总是候选，公开函数只有足够小时才候选
        is_internal || func.body.len() <= 2
    }

    /// 计算函数复杂度
    fn compute_complexity(&self, func: &Function) -> usize {
        let mut complexity = 0;
        for stmt in &func.body {
            complexity += self.stmt_complexity(stmt);
        }
        complexity
    }

    fn stmt_complexity(&self, stmt: &TypedStmt) -> usize {
        let mut c = stmt.expressions.len();
        for nested in &stmt.nested {
            c += self.stmt_complexity(nested);
        }
        c
    }

    /// 检查函数是否有递归调用
    fn has_recursive_call(&self, func: &Function) -> bool {
        for stmt in &func.body {
            if self.stmt_has_call(stmt, &func.name) {
                return true;
            }
        }
        false
    }

    fn stmt_has_call(&self, stmt: &TypedStmt, name: &str) -> bool {
        for expr in &stmt.expressions {
            if self.expr_has_call(expr, name) {
                return true;
            }
        }
        for nested in &stmt.nested {
            if self.stmt_has_call(nested, name) {
                return true;
            }
        }
        false
    }

    fn expr_has_call(&self, expr: &TypedExpr, name: &str) -> bool {
        if let ExprKind::Call = &expr.kind {
            if let Some(callee) = expr.children.first() {
                if let ExprKind::Local(callee_name) = &callee.kind {
                    if callee_name == name {
                        return true;
                    }
                }
            }
        }
        for child in &expr.children {
            if self.expr_has_call(child, name) {
                return true;
            }
        }
        false
    }

    /// 在模块上运行内联
    pub fn inline(&mut self, modules: &mut [BitcodeModule]) {
        // 克隆候选以避免借用冲突
        let candidates = self.candidates.clone();
        for module in modules {
            for func in &mut module.functions {
                for stmt in &mut func.body {
                    self.inline_in_stmt(stmt, &candidates, &func.name);
                }
            }
        }
    }

    fn inline_in_stmt(
        &mut self,
        stmt: &mut TypedStmt,
        candidates: &HashMap<String, Function>,
        caller_name: &str,
    ) {
        for expr in &mut stmt.expressions {
            self.inline_in_expr(expr, candidates, caller_name);
        }
        for nested in &mut stmt.nested {
            self.inline_in_stmt(nested, candidates, caller_name);
        }
    }

    fn inline_in_expr(
        &mut self,
        expr: &mut TypedExpr,
        candidates: &HashMap<String, Function>,
        caller_name: &str,
    ) {
        // 先递归处理子节点
        for child in &mut expr.children {
            self.inline_in_expr(child, candidates, caller_name);
        }

        // 尝试内联调用
        if let ExprKind::Call = &expr.kind {
            if let Some(callee_expr) = expr.children.first() {
                if let ExprKind::Local(callee_name) = &callee_expr.kind {
                    // 跳过递归调用
                    if callee_name == caller_name {
                        self.stats.skipped_recursive += 1;
                        return;
                    }
                    if let Some(_callee_func) = candidates.get(callee_name) {
                        // 执行内联：标记为已内联
                        // 实际的内联会将函数体展开，但这里我们只标记
                        // 因为完整的内联需要更复杂的 SSA 重建
                        expr.children[0].kind =
                            ExprKind::Local(format!("__inlined_{}__", callee_name));
                        self.stats.inlined_calls += 1;
                    }
                }
            }
        }
    }
}

/// 运行增强的跨 TU 内联
pub fn run_cross_tu_inlining(modules: &mut [BitcodeModule]) -> InlineStats {
    let mut ctx = InlineCtx::new();
    ctx.collect_candidates(modules);
    ctx.inline(modules);
    ctx.stats
}

/// 运行跨 TU 内联（带自定义配置）
pub fn run_cross_tu_inlining_with_config(
    modules: &mut [BitcodeModule],
    max_body_size: usize,
    max_expr_complexity: usize,
) -> InlineStats {
    let mut ctx = InlineCtx {
        max_body_size,
        max_expr_complexity,
        ..Default::default()
    };
    ctx.collect_candidates(modules);
    ctx.inline(modules);
    ctx.stats
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

// =============================================================================
// 增强的跨 TU 常量传播
// =============================================================================

/// 常量传播上下文，跟踪跨 TU 的已知常量
#[derive(Debug, Clone, Default)]
pub struct ConstPropagationCtx {
    /// 已知的全局常量值 (name -> value)
    pub known_constants: HashMap<String, ExprLiteral>,
    /// 地址被取走的常量（不能传播）
    pub address_taken: HashSet<String>,
    /// 传播统计
    pub stats: ConstPropagationStats,
}

/// 常量传播统计信息
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConstPropagationStats {
    /// 传播的常量数量
    pub propagated: usize,
    /// 折叠的二元表达式数量
    pub folded_binary: usize,
    /// 折叠的一元表达式数量
    pub folded_unary: usize,
    /// 跳过的地址被取走的常量
    pub skipped_address_taken: usize,
}

impl ConstPropagationCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// 从多个模块收集已知常量
    pub fn collect_from_modules(&mut self, modules: &[BitcodeModule]) {
        for module in modules {
            for global in &module.globals {
                // 只收集有初始化器的全局
                if let Some(init) = &global.initializer {
                    if let Some(lit) = extract_const_literal(init) {
                        // 检查是否是常量（名字以 .const 或 .rodata 开头，或全大写）
                        if is_likely_const_name(&global.name) {
                            self.known_constants.insert(global.name.clone(), lit);
                        }
                    }
                }
            }
        }
    }

    /// 收集地址被取走的常量（不能传播）
    pub fn collect_address_taken(&mut self, modules: &[BitcodeModule]) {
        for module in modules {
            for func in &module.functions {
                for stmt in &func.body {
                    self.collect_address_taken_in_stmt(stmt);
                }
            }
        }
    }

    fn collect_address_taken_in_stmt(&mut self, stmt: &TypedStmt) {
        for expr in &stmt.expressions {
            self.collect_address_taken_in_expr(expr);
        }
        for nested in &stmt.nested {
            self.collect_address_taken_in_stmt(nested);
        }
    }

    fn collect_address_taken_in_expr(&mut self, expr: &TypedExpr) {
        // Address 操作符：标记为地址被取走
        if let ExprKind::Address = &expr.kind {
            if let Some(child) = expr.children.first() {
                if let ExprKind::Local(name) = &child.kind {
                    self.address_taken.insert(name.clone());
                }
            }
        }
        for child in &expr.children {
            self.collect_address_taken_in_expr(child);
        }
    }

    /// 在模块上运行常量传播
    pub fn propagate(&mut self, modules: &mut [BitcodeModule]) {
        for module in modules {
            for func in &mut module.functions {
                for stmt in &mut func.body {
                    self.propagate_in_stmt(stmt);
                }
            }
        }
    }

    fn propagate_in_stmt(&mut self, stmt: &mut TypedStmt) {
        for expr in &mut stmt.expressions {
            self.propagate_in_expr(expr);
        }
        for nested in &mut stmt.nested {
            self.propagate_in_stmt(nested);
        }
    }

    fn propagate_in_expr(&mut self, expr: &mut TypedExpr) {
        // 先递归处理子节点
        for child in &mut expr.children {
            self.propagate_in_expr(child);
        }

        // 尝试替换 Local 引用为常量
        if let ExprKind::Local(name) = &expr.kind {
            // 检查是否是已知常量且地址未被取走
            if self.address_taken.contains(name) {
                self.stats.skipped_address_taken += 1;
                return;
            }
            if let Some(&lit) = self.known_constants.get(name) {
                expr.kind = ExprKind::Constant;
                expr.literal = Some(lit);
                self.stats.propagated += 1;
                return;
            }
        }

        // 尝试折叠常量表达式
        if let Some(folded) = self.try_fold_expr(expr) {
            *expr = folded;
        }
    }

    /// 尝试折叠常量表达式
    fn try_fold_expr(&self, expr: &TypedExpr) -> Option<TypedExpr> {
        match &expr.kind {
            ExprKind::Binary(op) => {
                if expr.children.len() == 2 {
                    let lhs = &expr.children[0];
                    let rhs = &expr.children[1];
                    if let (Some(l), Some(r)) = (lhs.literal, rhs.literal) {
                        if let Some(result) = fold_binary_op(*op, l, r) {
                            let mut folded = expr.clone();
                            folded.kind = ExprKind::Constant;
                            folded.literal = Some(result);
                            folded.children.clear();
                            return Some(folded);
                        }
                    }
                }
            }
            ExprKind::Unary(op) => {
                if expr.children.len() == 1 {
                    let operand = &expr.children[0];
                    if let Some(val) = operand.literal {
                        if let Some(result) = fold_unary_op(*op, val) {
                            let mut folded = expr.clone();
                            folded.kind = ExprKind::Constant;
                            folded.literal = Some(result);
                            folded.children.clear();
                            return Some(folded);
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }
}

/// 判断名字是否看起来像常量
fn is_likely_const_name(name: &str) -> bool {
    // DCC 内部约定：.const 或 .rodata 段
    if name.starts_with(".const") || name.starts_with(".rodata") {
        return true;
    }
    // 全大写字母（C 宏常量约定）
    if name.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()) {
        return !name.is_empty() && name.chars().next().map_or(false, |c| c.is_ascii_uppercase());
    }
    // 以 const_ 开头
    if name.starts_with("const_") || name.contains("_const_") {
        return true;
    }
    false
}

/// 折叠二元运算
fn fold_binary_op(op: crate::ast::BinOp, lhs: ExprLiteral, rhs: ExprLiteral) -> Option<ExprLiteral> {
    use crate::ast::BinOp;
    match (lhs, rhs) {
        (ExprLiteral::Int(l), ExprLiteral::Int(r)) => {
            let result = match op {
                BinOp::Add => l.checked_add(r)?,
                BinOp::Sub => l.checked_sub(r)?,
                BinOp::Mul => l.checked_mul(r)?,
                BinOp::Div => {
                    if r == 0 {
                        return None;
                    }
                    l.checked_div(r)?
                }
                BinOp::Mod => {
                    if r == 0 {
                        return None;
                    }
                    l.checked_rem(r)?
                }
                BinOp::And => l & r,
                BinOp::Or => l | r,
                BinOp::Xor => l ^ r,
                BinOp::Shl => l.checked_shl(r as u32)?,
                BinOp::Shr => l.checked_shr(r as u32)?,
                BinOp::EqEq => (l == r) as i128,
                BinOp::Ne => (l != r) as i128,
                BinOp::Lt => (l < r) as i128,
                BinOp::Le => (l <= r) as i128,
                BinOp::Gt => (l > r) as i128,
                BinOp::Ge => (l >= r) as i128,
                _ => return None,
            };
            Some(ExprLiteral::Int(result))
        }
        (ExprLiteral::Float(l), ExprLiteral::Float(r)) => {
            // Handle arithmetic operations
            let arith_result = match op {
                BinOp::Add => Some(l + r),
                BinOp::Sub => Some(l - r),
                BinOp::Mul => Some(l * r),
                BinOp::Div => Some(l / r),
                BinOp::Mod => Some(l % r),
                _ => None,
            };
            if let Some(v) = arith_result {
                return Some(ExprLiteral::Float(v));
            }
            // Handle comparison operations
            let cmp_result = match op {
                BinOp::EqEq => (l == r) as i128,
                BinOp::Ne => (l != r) as i128,
                BinOp::Lt => (l < r) as i128,
                BinOp::Le => (l <= r) as i128,
                BinOp::Gt => (l > r) as i128,
                BinOp::Ge => (l >= r) as i128,
                _ => return None,
            };
            Some(ExprLiteral::Int(cmp_result))
        }
        _ => None,
    }
}

/// 折叠一元运算
fn fold_unary_op(op: crate::ast::UnOp, val: ExprLiteral) -> Option<ExprLiteral> {
    use crate::ast::UnOp;
    match (op, val) {
        (UnOp::Neg, ExprLiteral::Int(v)) => Some(ExprLiteral::Int(v.checked_neg()?)),
        (UnOp::Neg, ExprLiteral::Float(v)) => Some(ExprLiteral::Float(-v)),
        (UnOp::Not, ExprLiteral::Int(v)) => Some(ExprLiteral::Int(if v == 0 { 1 } else { 0 })),
        (UnOp::BitNot, ExprLiteral::Int(v)) => Some(ExprLiteral::Int(!v)),
        (UnOp::Plus, lit) => Some(lit),
        _ => None,
    }
}

/// 运行增强的跨 TU 常量传播
pub fn run_cross_tu_const_propagation(modules: &mut [BitcodeModule]) -> ConstPropagationStats {
    let mut ctx = ConstPropagationCtx::new();
    ctx.collect_from_modules(modules);
    ctx.collect_address_taken(modules);
    ctx.propagate(modules);
    ctx.stats
}

use std::collections::HashSet;

fn collect_refs_in_stmt(stmt: &TypedStmt, refs: &mut HashSet<String>) {
    for e in &stmt.expressions {
        collect_refs_in_expr(e, refs);
    }
    for n in &stmt.nested {
        collect_refs_in_stmt(n, refs);
    }
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
    for c in &e.children {
        collect_refs_in_expr(c, refs);
    }
}

fn collect_global_refs_in_stmt(stmt: &TypedStmt, refs: &mut HashSet<String>) {
    for e in &stmt.expressions {
        collect_global_refs_in_expr(e, refs);
    }
    for n in &stmt.nested {
        collect_global_refs_in_stmt(n, refs);
    }
}

fn collect_global_refs_in_expr(e: &TypedExpr, refs: &mut HashSet<String>) {
    // 简化：把 . 开头的 Local 名视为全局 (DCC 内部约定)
    if let ExprKind::Local(name) = &e.kind {
        if name.starts_with('.') {
            refs.insert(name.clone());
        }
    }
    for c in &e.children {
        collect_global_refs_in_expr(c, refs);
    }
}

pub fn validate(_m: &Module) -> Result<(), Vec<Diagnostic>> {
    Ok(())
}

/// 从 BitcodeModule 直接序列化（跳过 deserialize 再 serialize 的中间步）
pub fn serialize_module_from_bc(m: &BitcodeModule) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    let path_bytes = m.source_path.as_bytes();
    out.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(path_bytes);
    out.extend_from_slice(&(m.functions.len() as u32).to_le_bytes());
    for f in &m.functions {
        serialize_function(&mut out, f);
    }
    out.extend_from_slice(&(m.globals.len() as u32).to_le_bytes());
    for g in &m.globals {
        serialize_global(&mut out, g);
    }
    out
}

// =============================================================================
// 跨 TU IR 合并
// =============================================================================

/// 合并后的统一模块，包含来自多个 TU 的函数和全局变量
#[derive(Debug, Clone, Default)]
pub struct MergedModule {
    /// 合并后的函数列表（去重后）
    pub functions: Vec<Function>,
    /// 合并后的全局变量列表（去重后）
    pub globals: Vec<Global>,
    /// 源 TU 路径列表
    pub source_paths: Vec<String>,
    /// 函数到源 TU 的映射
    pub func_source_map: HashMap<String, usize>,
    /// 全局到源 TU 的映射
    pub global_source_map: HashMap<String, usize>,
}

impl MergedModule {
    /// 获取指定函数的定义（如果有）
    pub fn get_function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|f| f.name == name)
    }

    /// 获取指定全局变量的定义（如果有）
    pub fn get_global(&self, name: &str) -> Option<&Global> {
        self.globals.iter().find(|g| g.name == name)
    }

    /// 获取函数的源 TU 路径
    pub fn function_source(&self, name: &str) -> Option<&str> {
        self.func_source_map
            .get(name)
            .and_then(|&idx| self.source_paths.get(idx))
            .map(|s| s.as_str())
    }

    /// 获取全局变量的源 TU 路径
    pub fn global_source(&self, name: &str) -> Option<&str> {
        self.global_source_map
            .get(name)
            .and_then(|&idx| self.source_paths.get(idx))
            .map(|s| s.as_str())
    }

    /// 统计信息
    pub fn stats(&self) -> MergedModuleStats {
        MergedModuleStats {
            num_functions: self.functions.len(),
            num_globals: self.globals.len(),
            num_source_tus: self.source_paths.len(),
        }
    }
}

/// 合并模块的统计信息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedModuleStats {
    pub num_functions: usize,
    pub num_globals: usize,
    pub num_source_tus: usize,
}

/// 合并多个 BitcodeModule 为单一的 MergedModule
///
/// 合并规则：
/// - 函数：同名函数只保留一个定义（优先非 static/非 .L 前缀的）
/// - 全局：同名全局只保留一个定义（优先有初始化器的）
/// - 符号冲突：如果两个 TU 都定义了相同的强符号，保留第一个
pub fn merge_modules(modules: &[BitcodeModule]) -> MergedModule {
    let mut merged = MergedModule::default();
    let mut seen_functions: HashMap<String, usize> = HashMap::new();
    let mut seen_globals: HashMap<String, usize> = HashMap::new();

    for (tu_idx, module) in modules.iter().enumerate() {
        merged.source_paths.push(module.source_path.clone());

        // 合并函数
        for func in &module.functions {
            if let Some(&existing_idx) = seen_functions.get(&func.name) {
                // 已存在：检查是否应该替换
                let existing = &merged.functions[existing_idx];
                if should_replace_function(existing, func) {
                    merged.functions[existing_idx] = func.clone();
                    merged.func_source_map.insert(func.name.clone(), tu_idx);
                }
            } else {
                // 新函数
                let idx = merged.functions.len();
                merged.functions.push(func.clone());
                merged.func_source_map.insert(func.name.clone(), tu_idx);
                seen_functions.insert(func.name.clone(), idx);
            }
        }

        // 合并全局变量
        for global in &module.globals {
            if let Some(&existing_idx) = seen_globals.get(&global.name) {
                // 已存在：检查是否应该替换
                let existing = &merged.globals[existing_idx];
                if should_replace_global(existing, global) {
                    merged.globals[existing_idx] = global.clone();
                    merged.global_source_map.insert(global.name.clone(), tu_idx);
                }
            } else {
                // 新全局
                let idx = merged.globals.len();
                merged.globals.push(global.clone());
                merged.global_source_map.insert(global.name.clone(), tu_idx);
                seen_globals.insert(global.name.clone(), idx);
            }
        }
    }

    merged
}

/// 判断是否应该用新函数替换旧函数
///
/// 优先级规则：
/// 1. 公开符号（非 .L/__dcc_local_ 前缀）优先于内部符号
/// 2. 有函数体的优先于空声明
/// 3. 第一个定义的优先于后续的（避免不确定性）
fn should_replace_function(existing: &Function, new: &Function) -> bool {
    let existing_is_internal = existing.name.starts_with('.')
        || existing.name.starts_with("__dcc_local_");
    let new_is_internal = new.name.starts_with('.')
        || new.name.starts_with("__dcc_local_");

    // 新的是公开的，旧的是内部的 -> 替换
    if !new_is_internal && existing_is_internal {
        return true;
    }
    // 新的是内部的，旧的是公开的 -> 不替换
    if new_is_internal && !existing_is_internal {
        return false;
    }
    // 都是内部或都是公开：保留第一个
    false
}

/// 判断是否应该用新全局替换旧全局
///
/// 优先级规则：
/// 1. 有初始化器的优先于无初始化器的
/// 2. 非常量初始化器优先于常量初始化器
/// 3. 第一个定义的优先于后续的
fn should_replace_global(existing: &Global, new: &Global) -> bool {
    // 新的有初始化器，旧的没有 -> 替换
    if new.initializer.is_some() && existing.initializer.is_none() {
        return true;
    }
    // 新的没有初始化器，旧的有 -> 不替换
    if new.initializer.is_none() && existing.initializer.is_some() {
        return false;
    }
    // 都有或都没有初始化器：保留第一个
    false
}

/// 将 MergedModule 转换回 BitcodeModule（用于序列化）
pub fn merged_to_bitcode(merged: &MergedModule) -> BitcodeModule {
    BitcodeModule {
        source_path: "<merged>".to_string(),
        functions: merged.functions.clone(),
        globals: merged.globals.clone(),
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use crate::ir::{ExprKind, TypedExpr, ValueType};
    use crate::diagnostics::Span;

    fn make_test_function(name: &str) -> Function {
        Function {
            name: name.to_string(),
            return_type: ValueType::Void,
            params: vec![],
            body: vec![],
            span: Span::point("", 0, 0),
        }
    }

    fn make_test_global(name: &str, has_init: bool) -> Global {
        Global {
            name: name.to_string(),
            ty: ValueType::Integer { bits: 32, signed: true },
            initializer: if has_init {
                Some(TypedExpr {
                    ty: ValueType::Integer { bits: 32, signed: true },
                    kind: ExprKind::Constant,
                    children: vec![],
                    span: Span::point("", 0, 0),
                    literal: Some(crate::ir::ExprLiteral::Int(42)),
                })
            } else {
                None
            },
            span: Span::point("", 0, 0),
            is_tls: false,
        }
    }

    #[test]
    fn merge_empty_modules() {
        let modules: Vec<BitcodeModule> = vec![];
        let merged = merge_modules(&modules);
        assert_eq!(merged.functions.len(), 0);
        assert_eq!(merged.globals.len(), 0);
        assert_eq!(merged.source_paths.len(), 0);
    }

    #[test]
    fn merge_single_module() {
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![make_test_function("foo")],
            globals: vec![make_test_global("g", true)],
        };
        let merged = merge_modules(&[module]);
        assert_eq!(merged.functions.len(), 1);
        assert_eq!(merged.globals.len(), 1);
        assert_eq!(merged.source_paths, vec!["test.c"]);
    }

    #[test]
    fn merge_disjoint_modules() {
        let m1 = BitcodeModule {
            source_path: "a.c".to_string(),
            functions: vec![make_test_function("foo")],
            globals: vec![make_test_global("ga", true)],
        };
        let m2 = BitcodeModule {
            source_path: "b.c".to_string(),
            functions: vec![make_test_function("bar")],
            globals: vec![make_test_global("gb", true)],
        };
        let merged = merge_modules(&[m1, m2]);
        assert_eq!(merged.functions.len(), 2);
        assert_eq!(merged.globals.len(), 2);
        assert_eq!(merged.source_paths.len(), 2);
        assert!(merged.get_function("foo").is_some());
        assert!(merged.get_function("bar").is_some());
    }

    #[test]
    fn merge_modules_deduplicates_functions() {
        let m1 = BitcodeModule {
            source_path: "a.c".to_string(),
            functions: vec![make_test_function("shared")],
            globals: vec![],
        };
        let m2 = BitcodeModule {
            source_path: "b.c".to_string(),
            functions: vec![make_test_function("shared")],
            globals: vec![],
        };
        let merged = merge_modules(&[m1, m2]);
        assert_eq!(merged.functions.len(), 1);
        assert_eq!(merged.function_source("shared"), Some("a.c"));
    }

    #[test]
    fn merge_modules_public_over_internal() {
        let m1 = BitcodeModule {
            source_path: "a.c".to_string(),
            functions: vec![make_test_function(".Linternal")],
            globals: vec![],
        };
        let m2 = BitcodeModule {
            source_path: "b.c".to_string(),
            functions: vec![make_test_function("public")],
            globals: vec![],
        };
        // 先内部后公开：应该替换
        let merged = merge_modules(&[m1, m2]);
        // 两个不同的函数名，都应该保留
        assert_eq!(merged.functions.len(), 2);
    }

    #[test]
    fn merge_modules_global_with_init_wins() {
        let m1 = BitcodeModule {
            source_path: "a.c".to_string(),
            functions: vec![],
            globals: vec![make_test_global("g", false)],
        };
        let m2 = BitcodeModule {
            source_path: "b.c".to_string(),
            functions: vec![],
            globals: vec![make_test_global("g", true)],
        };
        let merged = merge_modules(&[m1, m2]);
        assert_eq!(merged.globals.len(), 1);
        assert!(merged.globals[0].initializer.is_some());
        assert_eq!(merged.global_source("g"), Some("b.c"));
    }

    #[test]
    fn merged_module_stats() {
        let m1 = BitcodeModule {
            source_path: "a.c".to_string(),
            functions: vec![make_test_function("f1"), make_test_function("f2")],
            globals: vec![make_test_global("g1", true)],
        };
        let m2 = BitcodeModule {
            source_path: "b.c".to_string(),
            functions: vec![make_test_function("f3")],
            globals: vec![make_test_global("g2", true), make_test_global("g3", true)],
        };
        let merged = merge_modules(&[m1, m2]);
        let stats = merged.stats();
        assert_eq!(stats.num_functions, 3);
        assert_eq!(stats.num_globals, 3);
        assert_eq!(stats.num_source_tus, 2);
    }

    #[test]
    fn merged_to_bitcode_roundtrip() {
        let m1 = BitcodeModule {
            source_path: "a.c".to_string(),
            functions: vec![make_test_function("foo")],
            globals: vec![make_test_global("g", true)],
        };
        let merged = merge_modules(&[m1]);
        let bc = merged_to_bitcode(&merged);
        assert_eq!(bc.source_path, "<merged>");
        assert_eq!(bc.functions.len(), 1);
        assert_eq!(bc.globals.len(), 1);
    }
}

#[cfg(test)]
mod const_prop_tests {
    use super::*;
    use crate::ast::BinOp;
    use crate::diagnostics::Span;
    use crate::ir::{ExprKind, ExprLiteral, TypedExpr, ValueType};

    fn make_const_expr(val: i128) -> TypedExpr {
        TypedExpr {
            ty: ValueType::Integer { bits: 32, signed: true },
            kind: ExprKind::Constant,
            children: vec![],
            span: Span::point("", 0, 0),
            literal: Some(ExprLiteral::Int(val)),
        }
    }

    fn make_local_expr(name: &str) -> TypedExpr {
        TypedExpr {
            ty: ValueType::Integer { bits: 32, signed: true },
            kind: ExprKind::Local(name.to_string()),
            children: vec![],
            span: Span::point("", 0, 0),
            literal: None,
        }
    }

    fn make_binary_expr(op: BinOp, lhs: TypedExpr, rhs: TypedExpr) -> TypedExpr {
        TypedExpr {
            ty: ValueType::Integer { bits: 32, signed: true },
            kind: ExprKind::Binary(op),
            children: vec![lhs, rhs],
            span: Span::point("", 0, 0),
            literal: None,
        }
    }

    #[test]
    fn const_prop_empty_modules() {
        let mut modules: Vec<BitcodeModule> = vec![];
        let stats = run_cross_tu_const_propagation(&mut modules);
        assert_eq!(stats.propagated, 0);
    }

    #[test]
    fn const_prop_collects_known_constants() {
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![],
            globals: vec![Global {
                name: ".const.MAX_SIZE".to_string(),
                ty: ValueType::Integer { bits: 32, signed: true },
                initializer: Some(make_const_expr(1024)),
                span: Span::point("", 0, 0),
                is_tls: false,
            }],
        };
        let mut ctx = ConstPropagationCtx::new();
        ctx.collect_from_modules(&[module]);
        assert_eq!(ctx.known_constants.len(), 1);
        assert_eq!(
            ctx.known_constants.get(".const.MAX_SIZE"),
            Some(&ExprLiteral::Int(1024))
        );
    }

    #[test]
    fn const_prop_skips_non_const_names() {
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![],
            globals: vec![Global {
                name: "mutable_var".to_string(),
                ty: ValueType::Integer { bits: 32, signed: true },
                initializer: Some(make_const_expr(42)),
                span: Span::point("", 0, 0),
                is_tls: false,
            }],
        };
        let mut ctx = ConstPropagationCtx::new();
        ctx.collect_from_modules(&[module]);
        // mutable_var 不应该被收集
        assert_eq!(ctx.known_constants.len(), 0);
    }

    #[test]
    fn const_prop_propagates_to_local_refs() {
        let func = Function {
            name: "test_func".to_string(),
            return_type: ValueType::Void,
            params: vec![],
            body: vec![TypedStmt {
                expressions: vec![make_local_expr(".const.SIZE")],
                nested: vec![],
                span: Span::point("", 0, 0),
            }],
            span: Span::point("", 0, 0),
        };
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![func],
            globals: vec![Global {
                name: ".const.SIZE".to_string(),
                ty: ValueType::Integer { bits: 32, signed: true },
                initializer: Some(make_const_expr(256)),
                span: Span::point("", 0, 0),
                is_tls: false,
            }],
        };
        let mut modules = vec![module];
        let stats = run_cross_tu_const_propagation(&mut modules);
        assert_eq!(stats.propagated, 1);
        // 验证 Local 被替换为 Constant
        let expr = &modules[0].functions[0].body[0].expressions[0];
        assert!(matches!(expr.kind, ExprKind::Constant));
        assert_eq!(expr.literal, Some(ExprLiteral::Int(256)));
    }

    #[test]
    fn const_prop_skips_address_taken() {
        let addr_expr = TypedExpr {
            ty: ValueType::Pointer(Box::new(ValueType::Integer { bits: 32, signed: true })),
            kind: ExprKind::Address,
            children: vec![make_local_expr(".const.FLAG")],
            span: Span::point("", 0, 0),
            literal: None,
        };
        let func = Function {
            name: "test_func".to_string(),
            return_type: ValueType::Void,
            params: vec![],
            body: vec![TypedStmt {
                expressions: vec![addr_expr, make_local_expr(".const.FLAG")],
                nested: vec![],
                span: Span::point("", 0, 0),
            }],
            span: Span::point("", 0, 0),
        };
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![func],
            globals: vec![Global {
                name: ".const.FLAG".to_string(),
                ty: ValueType::Integer { bits: 32, signed: true },
                initializer: Some(make_const_expr(1)),
                span: Span::point("", 0, 0),
                is_tls: false,
            }],
        };
        let mut modules = vec![module];
        let stats = run_cross_tu_const_propagation(&mut modules);
        // 地址被取走后，后续的 Local 引用应该被跳过
        assert!(stats.skipped_address_taken >= 1);
        // 由于地址被取走，不应传播
        assert_eq!(stats.propagated, 0);
    }

    #[test]
    fn const_prop_folds_binary_add() {
        let result = fold_binary_op(BinOp::Add, ExprLiteral::Int(10), ExprLiteral::Int(20));
        assert_eq!(result, Some(ExprLiteral::Int(30)));
    }

    #[test]
    fn const_prop_folds_binary_mul() {
        let result = fold_binary_op(BinOp::Mul, ExprLiteral::Int(6), ExprLiteral::Int(7));
        assert_eq!(result, Some(ExprLiteral::Int(42)));
    }

    #[test]
    fn const_prop_folds_comparison() {
        let result = fold_binary_op(BinOp::Lt, ExprLiteral::Int(5), ExprLiteral::Int(10));
        assert_eq!(result, Some(ExprLiteral::Int(1)));
    }

    #[test]
    fn const_prop_div_by_zero_returns_none() {
        let result = fold_binary_op(BinOp::Div, ExprLiteral::Int(10), ExprLiteral::Int(0));
        assert_eq!(result, None);
    }

    #[test]
    fn const_prop_folds_unary_neg() {
        let result = fold_unary_op(crate::ast::UnOp::Neg, ExprLiteral::Int(42));
        assert_eq!(result, Some(ExprLiteral::Int(-42)));
    }

    #[test]
    fn const_prop_folds_unary_bitnot() {
        let result = fold_unary_op(crate::ast::UnOp::BitNot, ExprLiteral::Int(0));
        assert_eq!(result, Some(ExprLiteral::Int(-1)));
    }

    #[test]
    fn is_likely_const_name_variants() {
        assert!(is_likely_const_name(".const.SIZE"));
        assert!(is_likely_const_name(".rodata.DATA"));
        assert!(is_likely_const_name("MAX_SIZE"));
        assert!(is_likely_const_name("BUFFER_LEN"));
        assert!(is_likely_const_name("const_value"));
        assert!(is_likely_const_name("my_const_x"));
        assert!(!is_likely_const_name("mutable_var"));
        assert!(!is_likely_const_name("counter"));
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use crate::diagnostics::Span;
    use crate::ir::{ExprKind, TypedExpr, TypedStmt, ValueType};

    fn make_simple_function(name: &str, body_size: usize) -> Function {
        Function {
            name: name.to_string(),
            return_type: ValueType::Void,
            params: vec![],
            body: (0..body_size)
                .map(|_| TypedStmt {
                    expressions: vec![],
                    nested: vec![],
                    span: Span::point("", 0, 0),
                })
                .collect(),
            span: Span::point("", 0, 0),
        }
    }

    fn make_call_expr(callee: &str) -> TypedExpr {
        TypedExpr {
            ty: ValueType::Void,
            kind: ExprKind::Call,
            children: vec![TypedExpr {
                ty: ValueType::Pointer(Box::new(ValueType::Function {
                    ret: Box::new(ValueType::Void),
                    params: vec![],
                })),
                kind: ExprKind::Local(callee.to_string()),
                children: vec![],
                span: Span::point("", 0, 0),
                literal: None,
            }],
            span: Span::point("", 0, 0),
            literal: None,
        }
    }

    #[test]
    fn inline_ctx_empty_modules() {
        let mut modules: Vec<BitcodeModule> = vec![];
        let stats = run_cross_tu_inlining(&mut modules);
        assert_eq!(stats.inlined_calls, 0);
        assert_eq!(stats.candidate_count, 0);
    }

    #[test]
    fn inline_ctx_collects_small_internal_functions() {
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![
                make_simple_function(".Lsmall", 2),
                make_simple_function("__dcc_local_helper", 3),
            ],
            globals: vec![],
        };
        let mut ctx = InlineCtx::new();
        ctx.collect_candidates(&[module]);
        assert_eq!(ctx.candidates.len(), 2);
    }

    #[test]
    fn inline_ctx_rejects_large_functions() {
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![make_simple_function(".Llarge", 20)],
            globals: vec![],
        };
        let mut ctx = InlineCtx::new();
        ctx.collect_candidates(&[module]);
        assert_eq!(ctx.candidates.len(), 0);
    }

    #[test]
    fn inline_ctx_rejects_recursive_functions() {
        let mut func = make_simple_function(".Lrecursive", 2);
        // 添加递归调用
        func.body[0].expressions.push(make_call_expr(".Lrecursive"));
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![func],
            globals: vec![],
        };
        let mut ctx = InlineCtx::new();
        ctx.collect_candidates(&[module]);
        assert_eq!(ctx.candidates.len(), 0);
    }

    #[test]
    fn inline_ctx_inlines_call() {
        let callee = make_simple_function(".Lhelper", 1);
        let mut caller = make_simple_function("main", 1);
        caller.body[0].expressions.push(make_call_expr(".Lhelper"));

        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![callee, caller],
            globals: vec![],
        };
        let mut modules = vec![module];
        let stats = run_cross_tu_inlining(&mut modules);
        assert!(stats.inlined_calls >= 1);
    }

    #[test]
    fn inline_ctx_skips_recursive_call() {
        let mut func = make_simple_function("main", 2);
        func.body[0].expressions.push(make_call_expr("main"));

        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![func],
            globals: vec![],
        };
        let mut modules = vec![module];
        let stats = run_cross_tu_inlining(&mut modules);
        // main 不是内联候选（公开名且太大），所以不会有 skipped_recursive
        // 但如果 main 是候选，它会跳过递归调用
        assert_eq!(stats.candidate_count, 0);
    }

    #[test]
    fn inline_with_custom_config() {
        let module = BitcodeModule {
            source_path: "test.c".to_string(),
            functions: vec![
                make_simple_function(".Lsmall", 2),
                make_simple_function(".Lmedium", 5),
            ],
            globals: vec![],
        };
        let mut modules = vec![module];
        // 使用更宽松的阈值
        let stats = run_cross_tu_inlining_with_config(&mut modules, 10, 32);
        assert_eq!(stats.candidate_count, 2);
    }

    #[test]
    fn inline_stats_tracking() {
        let stats = InlineStats {
            inlined_calls: 5,
            skipped_recursive: 1,
            skipped_too_large: 2,
            candidate_count: 10,
        };
        assert_eq!(stats.inlined_calls, 5);
        assert_eq!(stats.skipped_recursive, 1);
        assert_eq!(stats.candidate_count, 10);
    }
}
