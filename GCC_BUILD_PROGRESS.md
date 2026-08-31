# GCC 编译进展报告

## 已完成的改进

### 1. C++ 解析器修复 (已完成)
- **const 前缀修复**: `const bidi::context &x` 中 const 不再阻止 qualified type 检测
- **双重 Reference 包装修复**: `const T&` 不再创建 `Reference(Reference(T))`
- **函数指针成员检测**: 支持关键字返回类型 (`bool (*f)(...)`, `int (*f)(...)`)
- **__attribute__ 后缀**: 函数指针成员支持 `__format__`/`__nonnull__` 等属性
- **elaborated type 修复**: `class mkdeps *fn()` 不再误判为类定义
- **返回类型检测**: `const struct line_map *fn()` 中的 struct/union/enum 正确处理
- **容错恢复**: 类/结构体成员解析失败时跳过到下一个成员而非中止整个类

### 2. C++ 降级修复 (已完成)
- **GCC 内建类型**: 注入 30+ glibc 类型前向声明 (`__fsid_t`, `__u_char` 等)
- **匿名聚合**: 支持 `union { ... } temp;` 等定义+实例一体成员
- **纯声明**: 类内成员函数声明输出原型而非空 weak 定义
- **union 支持**: 聚合声明使用 union 关键字 (成员重叠语义)
- **字符字面量修复**: `'"'` 不再被误判为字符串开头

### 3. 驱动修复 (已完成)
- **AST 降级容错**: 允许有解析错误时仍走 AST lower 路径
- **C++ 检测增强**: 添加 `::`, `operator`, 访问说明符, `new`/`delete` 检测

### 4. 全局成员函数调用转换 (已完成)
- **问题**: 基类成员函数在派生类构造函数中的调用未被转换
- **修复**: 同时检查 `*this` 和 ` *this` 模式, 全局替换裸成员函数调用

## 当前阻塞问题

### 1. lex.cc C 后端错误
**现象**: AST 降级成功但 C 后端编译失败 (~32 个错误)

**根因**:
- `linenum_arith_t` 类定义在降级中丢失, 构造函数调用 `_ZN15linenum_arith_tC1Ev` 无定义
- 部分系统头文件 typedef 在 AST 降级中被丢弃
- 构造函数初始化列表 (`: m_tabstop(tabstop), ...`) 未被正确降级

**影响**:
- lex.cc 无法编译
- 阻塞 libcpp 库构建

### 2. 构造函数初始化列表
**现象**: `ClassName(params) : member(init) { body }` 中初始化列表未被正确转换

**根因**: 解析器对构造函数初始化列表的容错恢复跳过整个构造函数

### 3. `= delete` / `= default`
**现象**: `label_text(const label_text &) = delete;` 导致解析错误

**根因**: 成员函数解析不支持 `= delete` / `= default` 后缀

## 统计信息
- 解析器错误: 从 67 个减少到 36 个 (lex.cc)
- C 后端错误: 32 个 (从 AST 降级代码产生)
- 已提交 commit: 3 个
  - `cpp/parser: fix multiple C++ parse failures blocking GCC lex.cc`
  - `cpp/lower: add GCC built-in type forward declarations and lowering fixes`
  - `driver: allow AST lowering with parser errors and improve C++ detection`

## 下一步建议

### 优先级 1: 修复 AST 降级中类定义丢失
- 调试 `linenum_arith_t` 等类为何在降级中丢失
- 检查 `parse_member_decl` 对构造函数初始化列表的处理

### 优先级 2: 添加 `= delete` / `= default` 支持
- 在 `parse_member_fn_inline` 中处理 `= delete` / `= default` 后缀

### 优先级 3: 修复构造函数初始化列表
- 解析 `: member(expr), ...` 初始化列表语法
- 降级为 C 中的赋值语句序列

## 编译命令
```bash
cd /root/gcc-15.3.0/build
make -j$(nproc)
```

### 单文件测试
```bash
cd /root/gcc-15.3.0/build/libcpp
/root/dcc/target/release/dcc -c -std=gnu++14 -O0 -g \
  -I../../libcpp -I. -I../../libcpp/../include -I../../libcpp/include \
  -DIN_GCC -DHAVE_CONFIG_H \
  ../../libcpp/lex.cc -o lex.o
```
