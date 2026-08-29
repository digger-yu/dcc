# GCC 编译进展报告

## 已完成的改进

### 1. C++ 类定义转换 (已完成)
- **构造函数转换**: `ClassName(params) : member(init) { body }` → `void _ctor_ClassName(struct ClassName *this_, params) { this_->member = init; ... body ... }`
- **成员函数提取**: `ret_type func(params) { body }` → `ret_type ClassName_func(struct ClassName *this_, params) { body }`
- **嵌套类型提升**: 类内定义的 `enum` 等类型提升到命名空间级别
- **嵌套大括号跟踪**: 修复了 enum/union 的 `}` 被误认为 struct 结束的问题

### 2. 全局成员函数调用转换 (已完成)
- **问题**: 基类成员函数在派生类构造函数中的调用未被转换
- **根因**: `fix_inline_member_calls` 中的模式匹配查找 ` *this` 但 `skip_ws()` 后指针指向 `*this`（无前导空格）
- **修复**: 
  - 同时检查 `*this` 和 ` *this` 模式
  - 使用字节数组比较而非字符串切片
  - 收集所有带 `struct S *this_` 参数的成员函数
  - 全局替换裸成员函数调用
- **效果**: 修复了 `set_escape_on_output` 在 `encoding_rich_location` 构造函数中的隐式声明问题
- **统计**: 在 macro.cc 中找到 43 个需要全局替换的成员函数

### 3. 提交的 Commit
```
cpp/lower: fix global member function call conversion
```

## 当前阻塞问题

### 1. uchar 类型未定义
**现象**: 
```
warning[W0002]: unknown type 'uchar' treated as 'int' (-fpermissive)
```

**根因**:
- `libcpp/internal.h` 第 692 行: `typedef unsigned char uchar;`
- 该 typedef 被 `#if !defined (HAVE_UCHAR) && !defined (IN_GCC)` 保护
- 编译时定义了 `-DIN_GCC`，导致 typedef 被跳过
- GCC 期望通过 `coretypes.h` 提供 `uchar`，但该头文件未被包含

**解决方案选项**:
1. 在预处理阶段添加 `uchar` typedef
2. 修复 preprocessor 的 include 缓存机制
3. 在 C++ 降级阶段注入 typedef

### 2. 文本降级逻辑严重错误
**现象**: 某些函数体被完全破坏
```c
// 错误的降级输出:
pfile->state.angled_headers , bracket;
char *fname , (((&(op)->ident))->str));

// 原始 C++ 代码:
pfile->state.angled_headers = false;
char *fname = NULL;
```

**根因**: 文本-based C++ 降级逻辑在某些情况下严重破坏代码结构

**影响**: 
- `builtin_has_include_1` 函数体被破坏
- 多处出现类似的语法错误
- 这是系统性问题，需要深入调试降级逻辑

### 3. __auto_type 未转换
**现象**:
```c
__auto_type sav_padding = pfile->state.directive_wants_padding;
```

**根因**: GCC 扩展 `__auto_type` 未被转换为具体类型

**解决方案**: 需要类型推导或转换为 `auto` 的 C 等价物

### 4. C++ 函数式转换语法
**现象**: C++ functional cast `Type(expr)` 未被正确转换

**示例**: 
```cpp
char *fname = char *((&(op)->ident))->str;  // C++
```
应转换为:
```c
char *fname = (char *)((&(op)->ident))->str;  // C
```

## 下一步建议

### 优先级 1: 修复文本降级逻辑
当前的文本-based 降级存在严重 bug，导致代码被破坏。需要：
1. 添加详细的调试输出，定位代码破坏的位置
2. 检查 `clean_struct_bodies` 函数的逻辑
3. 考虑切换到 AST-based 降级路径（如果可能）

### 优先级 2: 解决 uchar typedef 问题
最简单的方案是在预处理输出中注入：
```c
#ifndef HAVE_UCHAR
typedef unsigned char uchar;
#endif
```

### 优先级 3: 添加 __auto_type 支持
在 `strip_cpp_keywords` 或 `apply_post_lowering_conversions` 中添加转换

### 优先级 4: 完善 C++ 转换
- 函数式转换语法
- 其他 C++ 特有语法

## 技术细节

### 修改的文件
- `/root/dcc/src/cpp/lower.rs`: 
  - 添加 `nested_brace_depth` 跟踪
  - 添加 `member_fn_names` 收集
  - 修复构造函数转换
  - 修复全局成员函数调用转换

### 测试文件
- `/root/gcc-15.3.0/libcpp/macro.cc`: 目标编译文件
- 包含 `vaopt_state` 类（构造函数、成员函数、嵌套枚举）
- 包含 `encoding_rich_location` 类（继承自 `rich_location`）

### 编译命令
```bash
cd /root/gcc-15.3.0/build/build-x86_64-pc-linux-gnu/libcpp
/root/dcc/target/release/dcc -c -std=gnu++14 -O0 \
  -I. -I../../../libcpp -I../../../libcpp/include \
  -I../../../include -I../../../gcc -I.. \
  -DIN_GCC -DHAVE_CONFIG_H \
  ../../../libcpp/macro.cc -o macro.o
```

## 统计信息
- macro.cc 总行数: 4246
- 降级后 C 文件行数: 23429
- 发现的成员函数: 43 个
- 当前编译错误: ~20 个（主要是语法错误）
- 当前编译警告: ~60 个（主要是 uchar 类型）
