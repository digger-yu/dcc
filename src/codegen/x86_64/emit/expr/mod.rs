// src/codegen/x86_64/emit/expr/mod.rs
// Expression code generation submodules for x86_64 backend.
//
// 各 arm 搬迁进度记录于 mod.rs 的 `gen_expr` 委托中。旧影子 dispatcher
// `pub fn gen_expr(cg, e, dst)` 及 `unimplemented!()` 存根自由函数在
// batch 4 逐 arm 搬迁过程中已彻底无引用，一并作废删除。

pub mod aggregate;
pub mod complex;
pub mod dispatch;
pub mod float;
pub mod int_ops;
pub mod lvalue;
pub mod utils;
pub mod va_arg_store;
pub mod vector;

pub mod call;
pub mod cast;
pub mod compound;
pub mod increment;
pub mod literal;
pub mod va_arg;
