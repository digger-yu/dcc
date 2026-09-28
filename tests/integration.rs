// tests/integration.rs — 唯一 cargo integration target 壳。
// 全部测试按主题位于 tests/integration/*.rs，由本壳以下方 15 条包含指令文本级粘贴聚合，
// 公共助手位于 tests/common/harness.rs。子文件内不得再写 use/mod/fn main，
// 导入权归本壳（§5.2 规则 2）。
use dcc::codegen::shared::elf;
use dcc::cpp::mangle::MangleType;
use dcc::cpp::parser::{Access, CppDecl, CppParser, TemplateArgs, TemplateParam};
use dcc::cpp::sema::CxxSymbolKind;
use dcc::cpp::template::TemplateRegistry;
use dcc::link::target_clones::{TargetClonesFunc, TargetClonesTable, TargetVariant};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

#[path = "common/harness.rs"]
mod harness;
use harness::*;

include!("integration/c_lang.rs");

include!("integration/c_asm.rs");

include!("integration/c_kernel.rs");

include!("integration/c_attr_builtin.rs");

include!("integration/c_pp_c23.rs");

include!("integration/c_abi_vararg.rs");

include!("integration/c_link_debug.rs");

include!("integration/c_cross.rs");

include!("integration/c_sanitizer.rs");

include!("integration/cpp_lang.rs");

include!("integration/cpp_stdlib.rs");

include!("integration/cpp_runtime.rs");

include!("integration/opt.rs");

include!("integration/xfail.rs");

include!("integration/glue_misc.rs");
