# Phase 0-1 实现进度报告

## 已完成 (G7 - Trailing Return Type Parser)

### ✅ G7: Trailing return type `auto f() -> Ret`

**修改文件**: 
- `src/cpp/parser.rs`: 在 `parse_free_fn()` 函数中添加了 trailing return type 解析
- `src/cpp/lower.rs`: 更新 FreeFn pattern match 以包含 new field

**实现细节**:
```rust
// M30.5: trailing return type `auto f() -> Ret`
let mut trailing_ret_type: Option<MangleType> = None;
if self.at(CppTok::C(Tok {
    kind: TokKind::Arrow,  // 使用正确的 Token: Arrow
    ..
})) {
    self.bump(); // consume `->`
    let ret_ty = self.parse_mangle_type()?;
    trailing_ret_type = Some(ret_ty);
}
```

**状态**: ✅ Parser 部分已实现 (语法解析完成)
**待办**: Lowering & Native path 需要完整集成 trailing_ret_type 字段

---

## 计划中的 Phase 0 特性 (待实现)

### G8: static_assert semantics
- 需要在 `sema.rs` 和 `cpp_const_eval.rs` 中实现
- 必须 EVALUATE 条件为常量时判断真假

### G9: inline namespace  
- parser: 识别 `inline namespace ns {}`
- sema: lookup rules 提升成员到外层作用域

### G10: extern template suppression
- parser: 添加 `extern template` 声明解析
- sema: 抑制隐式实例化

### G19: Attribute whitelist ([[no_unique_address]])
- lower.rs: 不再剥离白名单属性
- abi_layout: 处理 EBO 布局

---

## Phase 1 模板核心 (重大工程)

这些是**最关键但也最复杂**的特性：

### G12: Variadic templates + Parameter packs (核心)
- 类型包表示、sizeof...展开、pack expansion
- **依赖所有其他模板特性先行**

### G6: Fold expressions
- 左/右/一元折叠 `(args op ...)`
- 依赖 G12 参数包展开

### G11: Template template parameters
- `template<template<class> class C>`
- 较独立，可在 G12 之后实现

### G13: Full specialization `template<>`
- 显式特化优先匹配规则

### G14: Semantic template instantiation (Route A)
- 文本替换外壳 + sema 名字查找
- SFINAE 失败回退

### G16: SFINAE at type system level
- enable_if/void_t/requires 约束失败不报错
- 重载决议的关键

### G17: Variable templates (`_v`)
- `template<typename T> constexpr bool is_integral_v = ...`

---

## 实施建议

由于代码修改非常复杂且容易引入回归，建议：

### 阶段 1: 完成 G7-G10 (Phase 0 剩余)
1. 先完成 G7 lowering (整合现有修改)
2. 逐个实现 G8-G10 (每个可独立测试)
3. 跑现有 test suite 确保零回归

### 阶段 2: Phase 1 模板核心 (分批次)
**批 1** (基础):
- G12 参数包 (最复杂，但依赖最多)
- G13 全特化  
- G14 Route A(文本替换 + sema)

**批 2** (增强):
- G16 SFINAE
- G17 变量模板
- G11 模板模板参数

**批 3** (收尾):
- G6 折叠表达式

### 验证策略
每完成一批就要：
1. `cargo check --lib` 无编译错误
2. `./scripts/test_regression.sh` 或类似脚本
3. 测试真实 libstdc++ 头解析

---

## Next Steps

由于单次回复长度限制，建议**分步实施**：

1. **立即行动**: 完成 G7 lowering 并测试
2. **短期** (1-2 天): G8-G10
3. **中期** (1 周): Phase 1 批 1+ 批 2
4. **长期** (1-2 周): Phase 1 批 3 + 验证

是否继续？请告知下一步要实现的特性。
