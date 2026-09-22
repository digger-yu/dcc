// src/pratt.rs - Pratt 表达式解析器 (A/B 测试基础设施)
//
// 第一性原理:
//   经典 Pratt 算法把"运算符优先级"转化为 token 自带的"结合力"
//   (binding power). 解析主循环 parse_expr 根据 min_prec 与当前
//   token 的左结合力决定是否消费; nud 处理前缀 (null-denotation),
//   led 处理中缀/后缀 (left-denotation). 这把"递归下降 + 优先级
//   样板"压缩为数据驱动.
//
//   此模块与现有 parser 并存, 不替换. 目的:
//     * 作为 A/B 测试基础设施, 在受控 benchmark 下与原 parser
//       对照解析结果, 验证语义等价.
//     * 提供一个独立可读的表达式解析器, 便于教学与重构参考.
//
// suckless 准则:
//   * 不引入第三方依赖; 解析表是 const fn + match.
//   * 错误以 Diagnostic 而非 panic 报告, 便于集成测试断言.
//   * 此模块不调用现有 parser 私有符号; 只使用 ast/tokens/diagnostics
//     的公开 API, 保证可单独编译与测试.
//
// 公开 API:
//   * `PrattParser::parse_expression(tokens) -> Result<(Expr, Vec<Diagnostic>), ()>`
//   * 输入: 已经由 lexer 产生的 `Vec<Tok>`. 调用方负责 lex (维持
//     与现有 parser 入口的对称性).
//   * 输出: 单条 `ast::Expr` + 累积诊断 (非致命错误).
//
// 已知简化 (A/B 测试范围内可接受):
//   * 不做完整类型推导. Expr 节点的 `ty` 字段填合理默认值
//     (Type::Int / Type::Double / Type::Ptr(Type::Char)).
//     这是为 A/B 测试服务的, 不是生产语义.
//   * 不解析 sizeof / cast / compound literal. 这些属于声明或
//     类型上下文, 留给调用方扩展.

use crate::ast::{self, BinOp, Expr, Ident, Type, UnOp};
use crate::diagnostics::{Diagnostic, Span};
use crate::tokens::{Tok, TokKind};

/// Pratt 解析器: 持有 token 流与游标, 在调用 parse_expr 时推进.
pub struct PrattParser {
    tokens: Vec<Tok>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
}

impl PrattParser {
    /// 入口函数: 把已 lex 的 token 流解析为单条表达式.
    ///
    /// 设计要点:
    ///   * 此函数是 `PrattParser` 的静态门面, 方便上层像调用
    ///     普通函数一样使用, 不必显式构造.
    ///   * 返回 `Result<(Expr, Vec<Diagnostic>), ()>`:
    ///       - 成功: 解析出的 Expr 与非致命诊断 (例如未消费 token).
    ///       - 失败: 不可恢复的语法错误, 调用方应中止.
    ///   * 当前实现允许 token 流末尾保留 Eof; 也允许 Eof 之后还有
    ///     额外 token (留给 statement 解析后续使用).
    pub fn parse_expression(tokens: Vec<Tok>) -> Result<(ast::Expr, Vec<Diagnostic>), ()> {
        let mut pp = PrattParser {
            tokens,
            pos: 0,
            diagnostics: Vec::new(),
        };
        let expr = pp.parse_expr(0)?;
        Ok((expr, pp.diagnostics))
    }

    /// 偷看当前 token (不消费). 越界返回 Eof 哨兵.
    fn peek(&self) -> &Tok {
        self.tokens.get(self.pos).unwrap_or_else(|| {
            // Eof 哨兵: 每次构造而非 static, 因为 Span::file 是 Arc<str>
            // 且无法在 const 上下文中调用 Arc::from.
            static EOF_SLOT: std::sync::OnceLock<Tok> = std::sync::OnceLock::new();
            EOF_SLOT.get_or_init(|| Tok {
                kind: TokKind::Eof,
                span: Span::point("", 0, 0),
                text: String::new(),
            })
        })
    }

    /// 消费并返回当前 token. 越界返回 Eof 哨兵.
    fn advance(&mut self) -> Tok {
        if let Some(t) = self.tokens.get(self.pos).cloned() {
            self.pos += 1;
            t
        } else {
            Tok {
                kind: TokKind::Eof,
                span: Span::point("", 0, 0),
                text: String::new(),
            }
        }
    }

    /// 核心 Pratt 循环.
    ///
    /// 算法 (经典 Pratt):
    ///   1. 解析前缀 (nud): 字面量 / 标识符 / 括号 / 前缀一元.
    ///   2. 循环: 偷看下一 token; 查 `infix_binding_power`;
    ///      若 lbp >= min_prec, 则消费 operator 并调用 led 合并.
    ///   3. 没有可消费的 operator 时返回.
    fn parse_expr(&mut self, min_prec: u8) -> Result<ast::Expr, ()> {
        let first = self.advance();
        let mut left = self.nud(first)?;

        loop {
            let op = self.peek().clone();
            let (lbp, _rbp) = match Self::infix_binding_power(&op) {
                Some(bp) => bp,
                None => break,
            };
            if lbp < min_prec {
                break;
            }
            self.advance(); // 消费 operator
            left = self.led(left, op)?;
        }
        Ok(left)
    }

    // ========================================================================
    // 前缀 (nud): 原子 + 一元前缀
    // ========================================================================

    /// nud 入口: 按当前 token 类型分派.
    fn nud(&mut self, tok: Tok) -> Result<ast::Expr, ()> {
        match tok.kind {
            TokKind::Number => Ok(self.nud_int(tok)),
            TokKind::FloatLit => Ok(self.nud_float(tok)),
            TokKind::StringLit | TokKind::WideStringLit | TokKind::Char16StringLit
            | TokKind::Char32StringLit => Ok(self.nud_string(tok)),
            TokKind::CharLit | TokKind::WideCharLit | TokKind::Char16Lit
            | TokKind::Char32Lit => Ok(self.nud_char(tok)),
            TokKind::Ident => Ok(self.nud_ident(tok)),
            TokKind::LParen => self.nud_paren(tok),
            TokKind::Plus
            | TokKind::Minus
            | TokKind::Not
            | TokKind::Tilde
            | TokKind::Star
            | TokKind::Amp
            | TokKind::PlusPlus
            | TokKind::MinusMinus => self.nud_unary(tok),
            _ => {
                self.diagnostics.push(Diagnostic::err(
                    "E0001",
                    format!("unexpected token at start of expression: {:?}", tok.kind),
                    tok.span.clone(),
                ));
                Err(())
            }
        }
    }

    /// 整数字面量 nud.
    /// 简化解码: 十进制 / 0x.. / 0b.. / 0..(八进制). 后缀 U/L/LL 忽略.
    /// 类型默认 Type::Int; A/B 测试不需要完整推导.
    fn nud_int(&mut self, tok: Tok) -> ast::Expr {
        let v = parse_int_lit_simple(&tok.text);
        Expr::Int {
            v,
            span: tok.span.clone(),
            ty: Type::Int,
        }
    }

    /// 浮点字面量 nud.
    fn nud_float(&mut self, tok: Tok) -> ast::Expr {
        let raw = strip_imag_suffix(&tok.text);
        let v = raw.parse::<f64>().unwrap_or(0.0);
        Expr::Float {
            v,
            span: tok.span.clone(),
            ty: Type::Double,
        }
    }

    /// 字符串字面量 nud.
    /// 当前仅简单存储 ASCII 字节 (不含宽字符串拼接/转义解码). 字符串
    /// 末尾追加 NUL 以匹配 C 字面量语义; A/B 测试中调用方主要比较
    /// 结构形态而非内容.
    fn nud_string(&mut self, tok: Tok) -> ast::Expr {
        let mut data = parse_str_simple(&tok.text);
        if matches!(
            tok.kind,
            TokKind::WideStringLit | TokKind::Char16StringLit | TokKind::Char32StringLit
        ) {
            // 宽字符串: 用 4 字节 NUL 简化占位; 不参与 A/B 测试形态比较.
            data.extend([0u8; 4]);
            Expr::String {
                data,
                span: tok.span.clone(),
                ty: Type::Ptr(Box::new(Type::Int.into())),
            }
        } else {
            data.push(0);
            Expr::String {
                data,
                span: tok.span.clone(),
                ty: Type::Ptr(Box::new(Type::Char.into())),
            }
        }
    }

    /// 字符字面量 nud.
    /// 简化解码: 单字节 ASCII 字符或 \\n \\t 等标准转义; Unicode 视为
    /// 原字符值.
    fn nud_char(&mut self, tok: Tok) -> ast::Expr {
        let v = parse_char_lit_simple(&tok.text);
        Expr::Char {
            v: v as i128,
            span: tok.span.clone(),
            ty: Type::Int,
        }
    }

    /// 标识符 nud.
    fn nud_ident(&mut self, tok: Tok) -> ast::Expr {
        let name = tok.text.clone();
        let id = Ident {
            name,
            span: tok.span.clone(),
        };
        Expr::Ident {
            name: id,
            span: tok.span.clone(),
            ty: Type::Int,
        }
    }

    /// 括号表达式 nud: `(expr)`. 当前不解析复合字面量 `(type){...}`,
    /// 那是声明上下文语义.
    fn nud_paren(&mut self, tok: Tok) -> Result<ast::Expr, ()> {
        let _open = tok;
        let inner = self.parse_expr(0)?;
        let close = self.peek().clone();
        if close.kind != TokKind::RParen {
            self.diagnostics.push(Diagnostic::err(
                "E0001",
                format!("expected ')', got {:?}", close.kind),
                close.span.clone(),
            ));
            return Err(());
        }
        self.advance(); // 消费 ')'
        Ok(inner)
    }

    /// 前缀一元 nud: + - ! ~ * & ++ --.
    fn nud_unary(&mut self, tok: Tok) -> Result<ast::Expr, ()> {
        let op = match tok.kind {
            TokKind::Plus => UnOp::Plus,
            TokKind::Minus => UnOp::Neg,
            TokKind::Not => UnOp::Not,
            TokKind::Tilde => UnOp::BitNot,
            TokKind::Star => {
                // 解引用: Expr::Deref
                let operand = self.parse_expr(prefix_rbp())?;
                let e = Expr::Deref {
                    e: Box::new(operand),
                    span: tok.span.clone(),
                    ty: Type::Int,
                };
                return Ok(e);
            }
            TokKind::Amp => {
                // 取址: Expr::AddrOf
                let operand = self.parse_expr(prefix_rbp())?;
                let e = Expr::AddrOf {
                    e: Box::new(operand),
                    span: tok.span.clone(),
                    ty: Type::Ptr(Box::new(Type::Int.into())),
                };
                return Ok(e);
            }
            TokKind::PlusPlus => {
                let operand = self.parse_expr(prefix_rbp())?;
                let e = Expr::PreInc {
                    e: Box::new(operand),
                    span: tok.span.clone(),
                    ty: Type::Int,
                };
                return Ok(e);
            }
            TokKind::MinusMinus => {
                let operand = self.parse_expr(prefix_rbp())?;
                let e = Expr::PreDec {
                    e: Box::new(operand),
                    span: tok.span.clone(),
                    ty: Type::Int,
                };
                return Ok(e);
            }
            _ => unreachable!("nud_unary called with non-unary token"),
        };
        let operand = self.parse_expr(prefix_rbp())?;
        Ok(Expr::Unary {
            op,
            e: Box::new(operand),
            span: tok.span.clone(),
            ty: Type::Int,
        })
    }

    // ========================================================================
    // 中缀/后缀 (led): 二元, 赋值, 三元, 调用, 下标, 成员, 后置增减
    // ========================================================================

    /// led 入口: 按 operator 类型分派. 已消费 operator.
    fn led(&mut self, left: ast::Expr, tok: Tok) -> Result<ast::Expr, ()> {
        // 右结合运算符清单: 赋值, 三元. 用 lbp (而非 rbp) 递归, 使得
        // 同优先级运算符能继续绑定 (右结合语义).
        let right_assoc = matches!(
            tok.kind,
            TokKind::Assign
                | TokKind::PlusEq
                | TokKind::MinusEq
                | TokKind::StarEq
                | TokKind::SlashEq
                | TokKind::PercentEq
                | TokKind::AmpEq
                | TokKind::PipeEq
                | TokKind::CaretEq
                | TokKind::ShlEq
                | TokKind::ShrEq
                | TokKind::Question
        );
        let (lbp, rbp) = Self::infix_binding_power(&tok).unwrap_or((0, 0));
        let next_min_prec = if right_assoc { lbp } else { rbp };

        match tok.kind {
            // 后缀
            TokKind::PlusPlus => {
                // 后置 ++
                Ok(Expr::PostInc {
                    e: Box::new(left),
                    span: tok.span.clone(),
                    ty: Type::Int,
                })
            }
            TokKind::MinusMinus => Ok(Expr::PostDec {
                e: Box::new(left),
                span: tok.span.clone(),
                ty: Type::Int,
            }),
            TokKind::LParen => self.led_call(left, tok),
            TokKind::LBracket => self.led_index(left, tok),
            TokKind::Dot | TokKind::Arrow => self.led_member(left, tok),
            // 三元 cond ? then : else
            TokKind::Question => self.led_ternary(left, tok, lbp),
            // 复合赋值与普通赋值
            TokKind::Assign
            | TokKind::PlusEq
            | TokKind::MinusEq
            | TokKind::StarEq
            | TokKind::SlashEq
            | TokKind::PercentEq
            | TokKind::AmpEq
            | TokKind::PipeEq
            | TokKind::CaretEq
            | TokKind::ShlEq
            | TokKind::ShrEq => self.led_assign(left, tok, next_min_prec),
            // 逗号
            TokKind::Comma => self.led_comma(left, tok, next_min_prec),
            // 其余二元
            _ => self.led_binary(left, tok, next_min_prec),
        }
    }

    /// 二元 led: parse 右侧, 构造 Expr::Binop.
    fn led_binary(
        &mut self,
        left: ast::Expr,
        op_tok: Tok,
        next_min_prec: u8,
    ) -> Result<ast::Expr, ()> {
        let op = match tok_to_binop(op_tok.kind) {
            Some(op) => op,
            None => {
                self.diagnostics.push(Diagnostic::err(
                    "E0001",
                    format!("unsupported binary operator: {:?}", op_tok.kind),
                    op_tok.span.clone(),
                ));
                return Err(());
            }
        };
        let right = self.parse_expr(next_min_prec)?;
        let span = op_tok.span.clone();
        Ok(Expr::Binop {
            op,
            l: Box::new(left),
            r: Box::new(right),
            span,
            ty: Type::Int,
        })
    }

    /// 逗号 led: parse 右侧, 构造 Expr::Comma.
    fn led_comma(
        &mut self,
        left: ast::Expr,
        op_tok: Tok,
        next_min_prec: u8,
    ) -> Result<ast::Expr, ()> {
        let right = self.parse_expr(next_min_prec)?;
        let span = op_tok.span.clone();
        Ok(Expr::Comma {
            l: Box::new(left),
            r: Box::new(right),
            span,
            ty: Type::Int,
        })
    }

    /// 赋值 led: parse 右侧, 构造 Expr::Assign.
    /// 复合赋值 (+= 等) 把 op 映射到对应 BinOp; BinOp::Add 用作
    /// "赋值" 哨兵 (与现有 parser 约定一致).
    fn led_assign(
        &mut self,
        left: ast::Expr,
        op_tok: Tok,
        next_min_prec: u8,
    ) -> Result<ast::Expr, ()> {
        let _ = match op_tok.kind {
            TokKind::Assign => BinOp::Add, // 哨兵
            TokKind::PlusEq => BinOp::Add,
            TokKind::MinusEq => BinOp::Sub,
            TokKind::StarEq => BinOp::Mul,
            TokKind::SlashEq => BinOp::Div,
            TokKind::PercentEq => BinOp::Mod,
            TokKind::AmpEq => BinOp::And,
            TokKind::PipeEq => BinOp::Or,
            TokKind::CaretEq => BinOp::Xor,
            TokKind::ShlEq => BinOp::Shl,
            TokKind::ShrEq => BinOp::Shr,
            _ => {
                self.diagnostics.push(Diagnostic::err(
                    "E0001",
                    format!("not an assignment operator: {:?}", op_tok.kind),
                    op_tok.span.clone(),
                ));
                return Err(());
            }
        };
        let right = self.parse_expr(next_min_prec)?;
        let span = op_tok.span.clone();
        Ok(Expr::Assign {
            op: BinOp::Add, // 与现有 parser 约定: Expr::Assign.op 是哨兵 Add
            l: Box::new(left),
            r: Box::new(right),
            span,
            ty: Type::Int,
        })
    }

    /// 三元 led: cond ? then : else. then 与 else 都用 next_min_prec=lbp
    /// (右结合) 保证 `a ? b : c ? d : e` 解析为 `a ? b : (c ? d : e)`.
    fn led_ternary(
        &mut self,
        cond: ast::Expr,
        q_tok: Tok,
        right_min_prec: u8,
    ) -> Result<ast::Expr, ()> {
        // GNU extension: x ?: y 等价于 x ? x : y
        let then_branch = if self.peek().kind == TokKind::Colon {
            cond.clone()
        } else {
            self.parse_expr(0)?
        };
        let colon = self.advance();
        if colon.kind != TokKind::Colon {
            self.diagnostics.push(Diagnostic::err(
                "E0001",
                format!("expected ':' in ternary, got {:?}", colon.kind),
                colon.span.clone(),
            ));
            return Err(());
        }
        let else_branch = self.parse_expr(right_min_prec)?;
        let span = q_tok.span.clone();
        Ok(Expr::Cond {
            cond: Box::new(cond),
            then_: Box::new(then_branch),
            else_: Box::new(else_branch),
            span,
            ty: Type::Int,
        })
    }

    /// 函数调用 led: f(args). args 用逗号分隔 (bp=1).
    /// 注意: parse_expr 调用使用 min_prec=2, 让逗号 (lbp=1) 不被
    /// 吞入参数内. 但更高优先级 (如 ?: bp=3, 赋值 bp=2) 仍能绑定.
    fn led_call(&mut self, callee: ast::Expr, _lparen: Tok) -> Result<ast::Expr, ()> {
        let mut args = Vec::new();
        if self.peek().kind != TokKind::RParen {
            loop {
                // 使用 min_prec=2 防止把 "," 作为子表达式吞掉.
                let arg = self.parse_expr(2)?;
                args.push(arg);
                if self.peek().kind == TokKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        let close = self.advance();
        if close.kind != TokKind::RParen {
            self.diagnostics.push(Diagnostic::err(
                "E0001",
                format!("expected ')' to close call, got {:?}", close.kind),
                close.span.clone(),
            ));
            return Err(());
        }
        Ok(Expr::Call {
            callee: Box::new(callee),
            args,
            span: close.span.clone(),
            ty: Type::Int,
        })
    }

    /// 下标 led: arr[idx].
    fn led_index(&mut self, arr: ast::Expr, _lbracket: Tok) -> Result<ast::Expr, ()> {
        let idx = self.parse_expr(0)?;
        let close = self.advance();
        if close.kind != TokKind::RBracket {
            self.diagnostics.push(Diagnostic::err(
                "E0001",
                format!("expected ']' to close index, got {:?}", close.kind),
                close.span.clone(),
            ));
            return Err(());
        }
        Ok(Expr::Index {
            arr: Box::new(arr),
            idx: Box::new(idx),
            span: close.span.clone(),
            ty: Type::Int,
        })
    }

    /// 成员访问 led: a.field / a->field.
    fn led_member(&mut self, expr: ast::Expr, op_tok: Tok) -> Result<ast::Expr, ()> {
        let field_tok = self.advance();
        if field_tok.kind != TokKind::Ident {
            self.diagnostics.push(Diagnostic::err(
                "E0001",
                format!("expected field name after '{:?}', got {:?}", op_tok.kind, field_tok.kind),
                field_tok.span.clone(),
            ));
            return Err(());
        }
        let id = Ident {
            name: field_tok.text.clone(),
            span: field_tok.span.clone(),
        };
        let is_arrow = op_tok.kind == TokKind::Arrow;
        let span = op_tok.span.clone();
        Ok(Expr::Member {
            expr: Box::new(expr),
            field: id,
            span,
            ty: Type::Int,
            is_arrow,
        })
    }

    // ========================================================================
    // 静态表: 结合力
    // ========================================================================

    /// 前缀结合力. 当前所有前缀一元使用相同 rbp=14 (最高), 便于
    /// 后续扩展 (例如 sizeof / alignof).
    fn prefix_binding_power(_tok: &Tok) -> Option<((), u8)> {
        // 实际判断在 nud 中按 kind 进行; 这里为常量接口预留, 当前不调用.
        None
    }

    /// 中缀/后缀结合力 (左, 右).
    ///
    /// 优先级表 (从低到高):
    /// ```text
    ///   * Comma:          (1, 2)   左结合
    ///   * Assign (+= 等): (2, 3)   右结合 (lbp=2)
    ///   * Ternary (?:):   (3, 4)   右结合
    ///   * ||              (4, 5)   左结合
    ///   * &&              (5, 6)   左结合
    ///   * |               (6, 7)   左结合
    ///   * ^               (7, 8)   左结合
    ///   * &               (8, 9)   左结合 (注意: 与 prefix 区分由
    ///                                nud 中显式 TokKind 分派完成)
    ///   * == !=           (9, 10)  左结合
    ///   * < > <= >=      (10, 11)  左结合
    ///   * << >>          (11, 12)  左结合
    ///   * + -            (12, 13)  左结合
    ///   * * / %          (13, 14)  左结合
    ///   * Postfix/call/index/member
    ///                     (14, 15) 左结合 (后缀不递归)
    /// ```
    fn infix_binding_power(tok: &Tok) -> Option<(u8, u8)> {
        match tok.kind {
            TokKind::Comma => Some((1, 2)),
            TokKind::Assign
            | TokKind::PlusEq
            | TokKind::MinusEq
            | TokKind::StarEq
            | TokKind::SlashEq
            | TokKind::PercentEq
            | TokKind::AmpEq
            | TokKind::PipeEq
            | TokKind::CaretEq
            | TokKind::ShlEq
            | TokKind::ShrEq => Some((2, 3)),
            TokKind::Question => Some((3, 4)),
            TokKind::OrOr => Some((4, 5)),
            TokKind::AndAnd => Some((5, 6)),
            TokKind::Pipe => Some((6, 7)),
            TokKind::Caret => Some((7, 8)),
            TokKind::Amp => Some((8, 9)),
            TokKind::EqEq | TokKind::Ne => Some((9, 10)),
            TokKind::Lt | TokKind::Gt | TokKind::Le | TokKind::Ge => Some((10, 11)),
            TokKind::Shl | TokKind::Shr => Some((11, 12)),
            TokKind::Plus | TokKind::Minus => Some((12, 13)),
            TokKind::Star | TokKind::Slash | TokKind::Percent => Some((13, 14)),
            TokKind::LParen | TokKind::LBracket | TokKind::Dot | TokKind::Arrow
            | TokKind::PlusPlus | TokKind::MinusMinus => Some((14, 15)),
            _ => None,
        }
    }
}

/// 前缀一元递归调用所用的右结合力. 14 = 最高, 与 multiplicative
/// 同级, 符合 C 标准 (前缀一元优先级最高).
fn prefix_rbp() -> u8 {
    14
}

/// TokKind → BinOp 映射. 注意: 不处理赋值与三元, 由 led_* 单独处理.
fn tok_to_binop(kind: TokKind) -> Option<BinOp> {
    match kind {
        TokKind::Plus => Some(BinOp::Add),
        TokKind::Minus => Some(BinOp::Sub),
        TokKind::Star => Some(BinOp::Mul),
        TokKind::Slash => Some(BinOp::Div),
        TokKind::Percent => Some(BinOp::Mod),
        TokKind::Amp => Some(BinOp::And),
        TokKind::Pipe => Some(BinOp::Or),
        TokKind::Caret => Some(BinOp::Xor),
        TokKind::Shl => Some(BinOp::Shl),
        TokKind::Shr => Some(BinOp::Shr),
        TokKind::Lt => Some(BinOp::Lt),
        TokKind::Gt => Some(BinOp::Gt),
        TokKind::Le => Some(BinOp::Le),
        TokKind::Ge => Some(BinOp::Ge),
        TokKind::EqEq => Some(BinOp::EqEq),
        TokKind::Ne => Some(BinOp::Ne),
        TokKind::AndAnd => Some(BinOp::AndAnd),
        TokKind::OrOr => Some(BinOp::OrOr),
        _ => None,
    }
}

// ============================================================================
// 简易字面量解析 (A/B 测试用). 不复制 parser.rs 内部函数以避免模块耦合.
// ============================================================================

/// 整数字面量解码: 0x.. (16), 0b.. (2), 0.. (8), 否则 10.
/// 后缀 U/L/LL 全部忽略.
fn parse_int_lit_simple(s: &str) -> i128 {
    let t = strip_int_suffix(s);
    if let Some(r) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i128::from_str_radix(r, 16).unwrap_or(0)
    } else if let Some(r) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        i128::from_str_radix(r, 2).unwrap_or(0)
    } else if let Some(r) = t.strip_prefix("0").filter(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())) {
        i128::from_str_radix(r, 8).unwrap_or(0)
    } else {
        t.parse::<i128>().unwrap_or(0)
    }
}

/// 移除整数后缀 U/L/LL/i/j (C99 虚数后缀也忽略).
fn strip_int_suffix(s: &str) -> &str {
    s.trim_end_matches(|c: char| "uUlLiIjJ".contains(c))
}

/// 移除浮点虚数后缀 (末尾 i/I/j/J), 用于 nud_float.
fn strip_imag_suffix(s: &str) -> &str {
    if let Some(last) = s.chars().last() {
        if matches!(last, 'i' | 'I' | 'j' | 'J') {
            return &s[..s.len() - last.len_utf8()];
        }
    }
    s
}

/// 简易字符串字面量解码: 取首尾引号之间内容, 不解析转义 (A/B 测试
/// 不依赖字面量内容, 仅比较结构形态).
fn parse_str_simple(raw: &str) -> Vec<u8> {
    let inner = if raw.len() >= 2 { &raw[1..raw.len() - 1] } else { raw };
    inner.as_bytes().to_vec()
}

/// 简易字符字面量解码: 单字节 ASCII, \\n \\t 等标准转义.
fn parse_char_lit_simple(raw: &str) -> u32 {
    let inner = if raw.len() >= 3 { &raw[1..raw.len() - 1] } else { raw };
    let bytes = inner.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    if bytes[0] == b'\\' && bytes.len() >= 2 {
        match bytes[1] {
            b'n' => b'\n' as u32,
            b't' => b'\t' as u32,
            b'r' => b'\r' as u32,
            b'\\' => b'\\' as u32,
            b'\'' => b'\'' as u32,
            b'"' => b'"' as u32,
            b'0' => 0,
            _ => bytes[1] as u32,
        }
    } else {
        bytes[0] as u32
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod pratt_tests {
    use super::*;
    use crate::tokens::Lexer;

    /// 工具: 把源代码 lex 后送入 parse_expression.
    fn parse(src: &[u8]) -> ast::Expr {
        let mut lexer = Lexer::new(src, "test.c");
        let mut tokens = Vec::new();
        loop {
            let t = lexer.next_tok();
            let is_eof = t.kind == TokKind::Eof;
            tokens.push(t);
            if is_eof {
                break;
            }
        }
        let (expr, diags) = PrattParser::parse_expression(tokens)
            .expect("parse_expression should succeed");
        assert!(
            diags.is_empty(),
            "unexpected diagnostics: {:?}",
            diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
        );
        expr
    }

    #[test]
    fn simple_int_literal() {
        // 42 → Expr::Int { v: 42, ty: Int }
        let e = parse(b"42");
        match e {
            Expr::Int { v, ty, .. } => {
                assert_eq!(v, 42);
                assert_eq!(ty, Type::Int);
            }
            other => panic!("expected Int, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn simple_addition() {
        // 1 + 2 → Binop Add(1, 2)
        let e = parse(b"1 + 2");
        match e {
            Expr::Binop { op, l, r, .. } => {
                assert_eq!(op, BinOp::Add);
                match *l {
                    Expr::Int { v, .. } => assert_eq!(v, 1),
                    _ => panic!("left is not Int"),
                }
                match *r {
                    Expr::Int { v, .. } => assert_eq!(v, 2),
                    _ => panic!("right is not Int"),
                }
            }
            _ => panic!("expected Binop"),
        }
    }

    #[test]
    fn precedence_mul_over_add() {
        // 1 + 2 * 3 → Binop Add(1, Binop Mul(2, 3))
        let e = parse(b"1 + 2 * 3");
        match e {
            Expr::Binop { op, l, r, .. } => {
                assert_eq!(op, BinOp::Add);
                match *l {
                    Expr::Int { v, .. } => assert_eq!(v, 1),
                    _ => panic!("left is not Int"),
                }
                match *r {
                    Expr::Binop { op, l, r, .. } => {
                        assert_eq!(op, BinOp::Mul);
                        match (*l, *r) {
                            (Expr::Int { v: 2, .. }, Expr::Int { v: 3, .. }) => {}
                            _ => panic!("inner Binop args wrong"),
                        }
                    }
                    _ => panic!("right is not Binop"),
                }
            }
            _ => panic!("expected Binop"),
        }
    }

    #[test]
    fn right_associative_assignment() {
        // a = b = 5 → Assign(a, Assign(b, 5))
        let e = parse(b"a = b = 5");
        match e {
            Expr::Assign { l, r, .. } => {
                match *l {
                    Expr::Ident { ref name, .. } => assert_eq!(name.name, "a"),
                    _ => panic!("left is not Ident(a)"),
                }
                match *r {
                    Expr::Assign { l, r, .. } => {
                        match *l {
                            Expr::Ident { ref name, .. } => assert_eq!(name.name, "b"),
                            _ => panic!("inner left is not Ident(b)"),
                        }
                        match *r {
                            Expr::Int { v: 5, .. } => {}
                            _ => panic!("inner right is not Int(5)"),
                        }
                    }
                    _ => panic!("inner right is not Assign"),
                }
            }
            _ => panic!("expected Assign"),
        }
    }

    #[test]
    fn parens_override_precedence() {
        // (1 + 2) * 3 → Binop Mul(Binop Add(1, 2), 3)
        let e = parse(b"(1 + 2) * 3");
        match e {
            Expr::Binop { op, l, r, .. } => {
                assert_eq!(op, BinOp::Mul);
                match *l {
                    Expr::Binop { op, l, r, .. } => {
                        assert_eq!(op, BinOp::Add);
                        match (*l, *r) {
                            (Expr::Int { v: 1, .. }, Expr::Int { v: 2, .. }) => {}
                            _ => panic!("inner Binop args wrong"),
                        }
                    }
                    _ => panic!("left is not Binop"),
                }
                match *r {
                    Expr::Int { v: 3, .. } => {}
                    _ => panic!("right is not Int(3)"),
                }
            }
            _ => panic!("expected Binop"),
        }
    }

    #[test]
    fn unary_minus() {
        // -42 → Unary Neg(Int(42))
        let e = parse(b"-42");
        match e {
            Expr::Unary { op, e, .. } => {
                assert_eq!(op, UnOp::Neg);
                match *e {
                    Expr::Int { v: 42, .. } => {}
                    _ => panic!("operand is not Int(42)"),
                }
            }
            _ => panic!("expected Unary"),
        }
    }

    #[test]
    fn function_call() {
        // f(1, 2) → Call(Ident(f), [Int(1), Int(2)])
        let e = parse(b"f(1, 2)");
        match e {
            Expr::Call { callee, args, .. } => {
                match *callee {
                    Expr::Ident { ref name, .. } => assert_eq!(name.name, "f"),
                    _ => panic!("callee is not Ident(f)"),
                }
                assert_eq!(args.len(), 2);
                match (&args[0], &args[1]) {
                    (Expr::Int { v: 1, .. }, Expr::Int { v: 2, .. }) => {}
                    _ => panic!("args not Int(1), Int(2)"),
                }
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn array_index() {
        // a[0] → Index(Ident(a), Int(0))
        let e = parse(b"a[0]");
        match e {
            Expr::Index { arr, idx, .. } => {
                match *arr {
                    Expr::Ident { ref name, .. } => assert_eq!(name.name, "a"),
                    _ => panic!("arr is not Ident(a)"),
                }
                match *idx {
                    Expr::Int { v: 0, .. } => {}
                    _ => panic!("idx is not Int(0)"),
                }
            }
            _ => panic!("expected Index"),
        }
    }
}