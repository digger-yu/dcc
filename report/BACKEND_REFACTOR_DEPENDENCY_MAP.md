# x86_64.rs Module Dependencies

## Top-Level Imports
use crate::ast::ArraySize;
use crate::ast::*;
use crate::codegen::elf;
use crate::codegen::elf::{
use crate::const_eval::{self, ConstValue};
use crate::diagnostics::{Diagnostic, Span};
use crate::preproc::LineSpan;
use crate::target::Target;
use crate::semantics::{
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

## Current Module Directory Structure
src/codegen/x86_64/abi/call.rs
src/codegen/x86_64/abi/classes.rs
src/codegen/x86_64/abi/frame.rs
src/codegen/x86_64/abi/mod.rs
src/codegen/x86_64/asm/directive.rs
src/codegen/x86_64/asm/emit.rs
src/codegen/x86_64/asm/label.rs
src/codegen/x86_64/asm/mod.rs
src/codegen/x86_64/asm/operand.rs
src/codegen/x86_64/asm/template.rs
src/codegen/x86_64/emit/expr/binary.rs
src/codegen/x86_64/emit/expr/call.rs
src/codegen/x86_64/emit/expr/cast.rs
src/codegen/x86_64/emit/expr/compound.rs
src/codegen/x86_64/emit/expr/init.rs
src/codegen/x86_64/emit/expr/literal.rs
src/codegen/x86_64/emit/expr/lvalue.rs
src/codegen/x86_64/emit/expr/mod.rs
src/codegen/x86_64/emit/expr/unary.rs
src/codegen/x86_64/emit/mod.rs
src/codegen/x86_64/mem/addr.rs
src/codegen/x86_64/mem/global.rs
src/codegen/x86_64/mem/load_store.rs
src/codegen/x86_64/mem/mod.rs
src/codegen/x86_64/parallel/collector.rs
src/codegen/x86_64/parallel/mod.rs
src/codegen/x86_64/parallel/orchestration.rs
src/codegen/x86_64/special/int128.rs
src/codegen/x86_64/special/mod.rs
src/codegen/x86_64/special/sanitizer.rs
src/codegen/x86_64/special/tls.rs
src/codegen/x86_64/special/x87_floating_point.rs
