//! Parser for the embedded `run_code` Python subset.
//!
//! [`rustpython_parser`] turns the source into a real Python AST. Nothing
//! here starts Node.js or a system Python. Execution of that AST is the
//! sandbox in `eval`: only the listed statements run, and the only way out
//! to the workspace is `tools.*`.

use rustpython_parser::ast::{
    self, BoolOp, CmpOp, Constant, Expr as PyExpr, Mod, Operator, Ranged, Stmt as PyStmt, UnaryOp,
};
use rustpython_parser::{Mode, parse as parse_python};

#[derive(Clone, Debug)]
pub(super) struct Program {
    pub stmts: Vec<Spanned>,
}

/// One statement plus the user-source line it came from.
#[derive(Clone, Debug)]
pub(super) struct Spanned {
    pub line: u32,
    pub snippet: String,
    pub stmt: Stmt,
}

#[derive(Clone, Debug)]
pub(super) enum Stmt {
    Let {
        name: String,
        value: Expr,
    },
    Return(Option<Expr>),
    If {
        cond: Expr,
        then_body: Vec<Spanned>,
        else_body: Vec<Spanned>,
    },
    While {
        cond: Expr,
        body: Vec<Spanned>,
    },
    ForOf {
        /// One name, or several names from `for a, b in ...`.
        names: Vec<String>,
        iter: Expr,
        body: Vec<Spanned>,
    },
    /// `obj[key] = value` or `items[i] = value`.
    SetIndex {
        object: Expr,
        index: Expr,
        value: Expr,
    },
    /// `name += value` and the other augmented assigns.
    AugName {
        name: String,
        op: BinOp,
        value: Expr,
    },
    /// `obj[key] += value`.
    AugIndex {
        object: Expr,
        index: Expr,
        op: BinOp,
        value: Expr,
    },
    Try {
        body: Vec<Spanned>,
        catch_name: String,
        catch_body: Vec<Spanned>,
    },
    Throw(Expr),
    Unpack {
        names: Vec<String>,
        value: Expr,
    },
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub(super) enum Expr {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Ident(String),
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Unary {
        op: Unary,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Member {
        object: Box<Expr>,
        name: String,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
    },
    /// `body if test else orelse`.
    IfExp {
        test: Box<Expr>,
        body: Box<Expr>,
        orelse: Box<Expr>,
    },
    Template(Vec<TemplatePart>),
    /// `[elt for target in iter if ...]`. One generator, name target.
    ListComp {
        target: String,
        iter: Box<Expr>,
        elt: Box<Expr>,
        ifs: Vec<Expr>,
    },
}

#[derive(Clone, Debug)]
pub(super) enum TemplatePart {
    Lit(String),
    Expr(Expr),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Unary {
    Await,
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    In,
    NotIn,
}

const FIX: &str = "Rewrite that part and call run_code again. You can use assignment (including a, b = ... and obj[key] = value), if/else, for, while, try/except, lists, dicts, f-strings, a if c else b, len/range/str/int/enumerate/zip/sorted/min/max/sum, slices, string split/join/strip, and await tools.name(arg=value). await gather(...) overlaps read-only calls. No import, classes, lambda, or match. Python does not need to be installed.";

pub(super) fn parse(src: &str) -> Result<Program, String> {
    let wrapped = wrap_function_body(src);
    let module = parse_python(&wrapped, Mode::Module, "<run_code>")
        .map_err(|err| syntax_error(src, &err.to_string()))?;
    let Mod::Module(module) = module else {
        return Err(format!("expected a program. {FIX}"));
    };
    let Some(PyStmt::AsyncFunctionDef(func)) = module.body.first() else {
        return Err(format!("internal wrapper was not a function. {FIX}"));
    };
    if module.body.len() != 1 {
        return Err(format!("extra statements outside the program. {FIX}"));
    }
    Ok(Program {
        stmts: convert_body(src, &func.body)?,
    })
}

fn wrap_function_body(src: &str) -> String {
    let mut out = String::from("async def __mycode__():\n");
    if src.trim().is_empty() {
        out.push_str("    pass\n");
        return out;
    }
    for line in src.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn convert_body(src: &str, body: &[PyStmt]) -> Result<Vec<Spanned>, String> {
    let mut stmts = Vec::new();
    for stmt in body {
        if let Some(inner) = convert_stmt(src, stmt)? {
            stmts.push(spanned(src, stmt, inner));
        }
    }
    Ok(stmts)
}

fn spanned(src: &str, stmt: &PyStmt, inner: Stmt) -> Spanned {
    let prefix = "async def __mycode__():\n".len();
    let start = stmt.start().to_usize().saturating_sub(prefix);
    let (line, _column) = line_col(src, start);
    let snippet = src.lines().nth(line.saturating_sub(1)).unwrap_or("").trim();
    let snippet: String = snippet.chars().take(120).collect();
    Spanned {
        line: line as u32,
        snippet,
        stmt: inner,
    }
}

fn convert_stmt(src: &str, stmt: &PyStmt) -> Result<Option<Stmt>, String> {
    match stmt {
        PyStmt::Pass(_) => Ok(None),
        PyStmt::Return(node) => Ok(Some(Stmt::Return(
            node.value
                .as_ref()
                .map(|expr| convert_expr(expr))
                .transpose()?,
        ))),
        PyStmt::Assign(node) => convert_assign(&node.targets, &node.value),
        PyStmt::Expr(node) => Ok(Some(Stmt::Expr(convert_expr(&node.value)?))),
        PyStmt::If(node) => Ok(Some(Stmt::If {
            cond: convert_expr(&node.test)?,
            then_body: convert_body(src, &node.body)?,
            else_body: convert_body(src, &node.orelse)?,
        })),
        PyStmt::While(node) => {
            if !node.orelse.is_empty() {
                return Err(unsupported("while else"));
            }
            Ok(Some(Stmt::While {
                cond: convert_expr(&node.test)?,
                body: convert_body(src, &node.body)?,
            }))
        }
        PyStmt::For(node) => {
            if !node.orelse.is_empty() {
                return Err(unsupported("for else"));
            }
            Ok(Some(Stmt::ForOf {
                names: loop_names(&node.target)?,
                iter: convert_expr(&node.iter)?,
                body: convert_body(src, &node.body)?,
            }))
        }
        PyStmt::Try(node) => convert_try(src, node),
        PyStmt::Raise(node) => {
            let Some(exc) = node.exc.as_ref() else {
                return Err(unsupported("bare raise"));
            };
            Ok(Some(Stmt::Throw(convert_expr(exc)?)))
        }
        PyStmt::AugAssign(node) => convert_aug(&node.target, node.op, &node.value),
        PyStmt::AnnAssign(node) => {
            let Some(value) = node.value.as_deref() else {
                return Ok(None);
            };
            convert_assign(std::slice::from_ref(node.target.as_ref()), value)
        }
        PyStmt::Import(_) | PyStmt::ImportFrom(_) => {
            let here = spanned(src, stmt, Stmt::Expr(Expr::Null));
            Err(format!(
                "line {}: `{}` — import is not available in run_code. There is no module system, and Python does not need to be installed. Call tools directly, for example hits = await tools.grep(pattern=\"foo\", include=\"*.rs\"). Do not import os, json, or pathlib.",
                here.line, here.snippet
            ))
        }
        _ => Err(located_unsupported(src, stmt, "that statement")),
    }
}

fn loop_names(target: &PyExpr) -> Result<Vec<String>, String> {
    match target {
        PyExpr::Name(name) => Ok(vec![name.id.as_str().to_owned()]),
        PyExpr::Tuple(tuple) => unpack_names(&tuple.elts),
        PyExpr::List(list) => unpack_names(&list.elts),
        _ => Err(unsupported(
            "a for target must be a name or a tuple of names, as in `for i, item in enumerate(items)`",
        )),
    }
}

fn convert_assign(targets: &[PyExpr], value: &PyExpr) -> Result<Option<Stmt>, String> {
    if targets.len() != 1 {
        return Err(unsupported("chained assignment"));
    }
    let value = convert_expr(value)?;
    match &targets[0] {
        PyExpr::Name(name) => Ok(Some(Stmt::Let {
            name: name.id.as_str().to_owned(),
            value,
        })),
        PyExpr::Tuple(tuple) => unpack(&tuple.elts, value),
        PyExpr::List(list) => unpack(&list.elts, value),
        PyExpr::Subscript(node) => {
            if matches!(node.slice.as_ref(), PyExpr::Slice(_)) {
                return Err(unsupported(
                    "slice assignment. Assign one index, as in items[i] = value",
                ));
            }
            Ok(Some(Stmt::SetIndex {
                object: convert_expr(&node.value)?,
                index: convert_expr(&node.slice)?,
                value,
            }))
        }
        _ => Err(unsupported(
            "that assignment target. Use a name, a tuple of names (`a, b = ...`), or obj[key] = value",
        )),
    }
}

fn convert_aug(target: &PyExpr, op: Operator, value: &PyExpr) -> Result<Option<Stmt>, String> {
    let op = convert_operator(op)?;
    let value = convert_expr(value)?;
    match target {
        PyExpr::Name(name) => Ok(Some(Stmt::AugName {
            name: name.id.as_str().to_owned(),
            op,
            value,
        })),
        PyExpr::Subscript(node) => {
            if matches!(node.slice.as_ref(), PyExpr::Slice(_)) {
                return Err(unsupported("augmented slice assignment"));
            }
            Ok(Some(Stmt::AugIndex {
                object: convert_expr(&node.value)?,
                index: convert_expr(&node.slice)?,
                op,
                value,
            }))
        }
        _ => Err(unsupported(
            "that augmented assignment. Use name += value or obj[key] += value",
        )),
    }
}

fn unpack_names(elts: &[PyExpr]) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for elt in elts {
        let PyExpr::Name(name) = elt else {
            return Err(unsupported(
                "unpacking into anything but names. Write `a, b = await gather(...)`",
            ));
        };
        names.push(name.id.as_str().to_owned());
    }
    if names.is_empty() {
        return Err(unsupported("empty unpack"));
    }
    Ok(names)
}

fn unpack(elts: &[PyExpr], value: Expr) -> Result<Option<Stmt>, String> {
    Ok(Some(Stmt::Unpack {
        names: unpack_names(elts)?,
        value,
    }))
}

fn convert_try(src: &str, node: &ast::StmtTry) -> Result<Option<Stmt>, String> {
    if !node.orelse.is_empty() || !node.finalbody.is_empty() {
        return Err(unsupported("try else/finally"));
    }
    if node.handlers.len() != 1 {
        return Err(unsupported("one except handler"));
    }
    let ast::ExceptHandler::ExceptHandler(handler) = &node.handlers[0];
    Ok(Some(Stmt::Try {
        body: convert_body(src, &node.body)?,
        catch_name: handler
            .name
            .as_ref()
            .map(|name| name.as_str().to_owned())
            .unwrap_or_else(|| "error".to_owned()),
        catch_body: convert_body(src, &handler.body)?,
    }))
}

fn convert_expr(expr: &PyExpr) -> Result<Expr, String> {
    match expr {
        PyExpr::Constant(node) => convert_constant(&node.value),
        PyExpr::Name(node) => Ok(Expr::Ident(node.id.as_str().to_owned())),
        PyExpr::List(node) => Ok(Expr::Array(
            node.elts
                .iter()
                .map(convert_expr)
                .collect::<Result<_, _>>()?,
        )),
        PyExpr::Tuple(node) => Ok(Expr::Array(
            node.elts
                .iter()
                .map(convert_expr)
                .collect::<Result<_, _>>()?,
        )),
        PyExpr::Dict(node) => {
            let mut fields = Vec::new();
            for (key, value) in node.keys.iter().zip(node.values.iter()) {
                let Some(key) = key else {
                    return Err(unsupported("dict **spread"));
                };
                let key = match convert_expr(key)? {
                    Expr::Str(text) => text,
                    _ => return Err(unsupported("dict key must be a string")),
                };
                fields.push((key, convert_expr(value)?));
            }
            Ok(Expr::Object(fields))
        }
        PyExpr::Attribute(node) => Ok(Expr::Member {
            object: Box::new(convert_expr(&node.value)?),
            name: node.attr.as_str().to_owned(),
        }),
        PyExpr::Subscript(node) => convert_subscript(&node.value, &node.slice),
        PyExpr::Await(node) => Ok(Expr::Unary {
            op: Unary::Await,
            expr: Box::new(convert_expr(&node.value)?),
        }),
        PyExpr::Call(node) => convert_call(node),
        PyExpr::BinOp(node) => Ok(Expr::Binary {
            op: convert_operator(node.op)?,
            left: Box::new(convert_expr(&node.left)?),
            right: Box::new(convert_expr(&node.right)?),
        }),
        PyExpr::UnaryOp(node) => Ok(Expr::Unary {
            op: convert_unary(node.op)?,
            expr: Box::new(convert_expr(&node.operand)?),
        }),
        PyExpr::BoolOp(node) => {
            let op = match node.op {
                BoolOp::And => BinOp::And,
                BoolOp::Or => BinOp::Or,
            };
            let mut values = node.values.iter();
            let Some(first) = values.next() else {
                return Err(unsupported("empty boolean expression"));
            };
            let mut expr = convert_expr(first)?;
            for next in values {
                expr = Expr::Binary {
                    op,
                    left: Box::new(expr),
                    right: Box::new(convert_expr(next)?),
                };
            }
            Ok(expr)
        }
        PyExpr::Compare(node) => convert_compare(node),
        PyExpr::JoinedStr(node) => {
            let mut parts = Vec::new();
            for value in &node.values {
                match value {
                    PyExpr::Constant(constant) => {
                        let Expr::Str(text) = convert_constant(&constant.value)? else {
                            return Err(unsupported("f-string literal"));
                        };
                        parts.push(TemplatePart::Lit(text));
                    }
                    PyExpr::FormattedValue(value) => {
                        if value.format_spec.is_some() {
                            return Err(unsupported("f-string format spec"));
                        }
                        parts.push(TemplatePart::Expr(convert_expr(&value.value)?));
                    }
                    _ => return Err(unsupported("f-string part")),
                }
            }
            Ok(Expr::Template(parts))
        }
        PyExpr::ListComp(node) => convert_list_comp(node),
        PyExpr::IfExp(node) => Ok(Expr::IfExp {
            test: Box::new(convert_expr(&node.test)?),
            body: Box::new(convert_expr(&node.body)?),
            orelse: Box::new(convert_expr(&node.orelse)?),
        }),
        _ => Err(unsupported("that expression")),
    }
}

fn convert_subscript(value: &PyExpr, slice: &PyExpr) -> Result<Expr, String> {
    if let PyExpr::Slice(slice) = slice {
        if slice.step.is_some() {
            return Err(unsupported("slice step"));
        }
        let mut args = Vec::new();
        if let Some(lower) = &slice.lower {
            args.push(convert_expr(lower)?);
        } else {
            args.push(Expr::Num(0.0));
        }
        if let Some(upper) = &slice.upper {
            args.push(convert_expr(upper)?);
        }
        return Ok(Expr::Call {
            callee: Box::new(Expr::Member {
                object: Box::new(convert_expr(value)?),
                name: "slice".to_owned(),
            }),
            args,
            kwargs: Vec::new(),
        });
    }
    Ok(Expr::Index {
        object: Box::new(convert_expr(value)?),
        index: Box::new(convert_expr(slice)?),
    })
}

fn convert_call(node: &ast::ExprCall) -> Result<Expr, String> {
    if node.keywords.iter().any(|keyword| keyword.arg.is_none()) {
        return Err(unsupported("**kwargs"));
    }
    let callee = convert_expr(&node.func)?;
    let tool_call = matches!(
        callee,
        Expr::Member { ref object, .. } if matches!(object.as_ref(), Expr::Ident(name) if name == "tools")
    );
    if tool_call {
        let arg = if !node.keywords.is_empty() {
            if !node.args.is_empty() {
                return Err(unsupported(
                    "tool call with both positional and keyword arguments",
                ));
            }
            let mut fields = Vec::new();
            for keyword in &node.keywords {
                let name = keyword
                    .arg
                    .as_ref()
                    .map(|arg| arg.as_str().to_owned())
                    .unwrap_or_default();
                fields.push((name, convert_expr(&keyword.value)?));
            }
            Expr::Object(fields)
        } else if node.args.len() == 1 {
            convert_expr(&node.args[0])?
        } else if node.args.is_empty() {
            Expr::Object(Vec::new())
        } else {
            return Err(unsupported(
                "tool call. Use keyword arguments: tools.read(path=\"a.rs\")",
            ));
        };
        return Ok(Expr::Call {
            callee: Box::new(callee),
            args: vec![arg],
            kwargs: Vec::new(),
        });
    }
    let args = node
        .args
        .iter()
        .map(convert_expr)
        .collect::<Result<Vec<_>, _>>()?;
    let mut kwargs = Vec::new();
    for keyword in &node.keywords {
        let Some(name) = keyword.arg.as_ref() else {
            return Err(unsupported("**kwargs"));
        };
        kwargs.push((name.as_str().to_owned(), convert_expr(&keyword.value)?));
    }
    Ok(Expr::Call {
        callee: Box::new(callee),
        args,
        kwargs,
    })
}

fn convert_compare(node: &ast::ExprCompare) -> Result<Expr, String> {
    if node.ops.len() != 1 || node.comparators.len() != 1 {
        return Err(unsupported("chained comparison"));
    }
    let op = match node.ops[0] {
        CmpOp::Eq => BinOp::Eq,
        CmpOp::NotEq => BinOp::Ne,
        CmpOp::Lt => BinOp::Lt,
        CmpOp::LtE => BinOp::Le,
        CmpOp::Gt => BinOp::Gt,
        CmpOp::GtE => BinOp::Ge,
        CmpOp::In => BinOp::In,
        CmpOp::NotIn => BinOp::NotIn,
        CmpOp::Is | CmpOp::IsNot => return Err(unsupported("`is`. Use == or !=")),
    };
    Ok(Expr::Binary {
        op,
        left: Box::new(convert_expr(&node.left)?),
        right: Box::new(convert_expr(&node.comparators[0])?),
    })
}

fn convert_list_comp(node: &ast::ExprListComp) -> Result<Expr, String> {
    if node.generators.len() != 1 {
        return Err(unsupported("one for clause in a list comprehension"));
    }
    let generator = &node.generators[0];
    if generator.is_async {
        return Err(unsupported("async for in a comprehension"));
    }
    let ast::Expr::Name(target) = &generator.target else {
        return Err(unsupported("comprehension target must be a name"));
    };
    Ok(Expr::ListComp {
        target: target.id.as_str().to_owned(),
        iter: Box::new(convert_expr(&generator.iter)?),
        elt: Box::new(convert_expr(&node.elt)?),
        ifs: generator
            .ifs
            .iter()
            .map(convert_expr)
            .collect::<Result<_, _>>()?,
    })
}

fn convert_operator(op: Operator) -> Result<BinOp, String> {
    Ok(match op {
        Operator::Add => BinOp::Add,
        Operator::Sub => BinOp::Sub,
        Operator::Mult => BinOp::Mul,
        Operator::Div => BinOp::Div,
        Operator::Mod => BinOp::Rem,
        Operator::FloorDiv | Operator::Pow | Operator::MatMult => {
            return Err(unsupported("that arithmetic operator"));
        }
        _ => return Err(unsupported("bitwise operator")),
    })
}

fn convert_unary(op: UnaryOp) -> Result<Unary, String> {
    Ok(match op {
        UnaryOp::Not => Unary::Not,
        UnaryOp::USub => Unary::Neg,
        UnaryOp::UAdd => return Err(unsupported("unary +")),
        UnaryOp::Invert => return Err(unsupported("~")),
    })
}

fn convert_constant(value: &Constant) -> Result<Expr, String> {
    Ok(match value {
        Constant::None => Expr::Null,
        Constant::Bool(value) => Expr::Bool(*value),
        Constant::Str(value) => Expr::Str(value.clone()),
        Constant::Int(value) => {
            let text = value.to_string();
            let number = text
                .parse::<f64>()
                .map_err(|_| unsupported("integer that does not fit a number"))?;
            Expr::Num(number)
        }
        Constant::Float(value) => Expr::Num(*value),
        _ => return Err(unsupported("that constant")),
    })
}

fn syntax_error(src: &str, raw: &str) -> String {
    let prefix = "async def __mycode__():\n".len();
    let located = byte_offset(raw).and_then(|offset| {
        let user = offset.checked_sub(prefix)?;
        let (line, _column) = line_col(src, user);
        let snippet = src.lines().nth(line.saturating_sub(1)).unwrap_or("").trim();
        let snippet: String = snippet.chars().take(120).collect();
        Some(format!("line {line}: `{snippet}` — syntax error: {raw}"))
    });
    format!(
        "{}. {FIX}",
        located.unwrap_or_else(|| format!("syntax error: {raw}"))
    )
}

fn located_unsupported(src: &str, stmt: &PyStmt, what: &str) -> String {
    let here = spanned(src, stmt, Stmt::Expr(Expr::Null));
    format!(
        "line {}: `{}` — {what} is not available in this embedded Python. {FIX}",
        here.line, here.snippet
    )
}

fn byte_offset(raw: &str) -> Option<usize> {
    let marker = "byte offset ";
    let start = raw.rfind(marker)? + marker.len();
    let digits: String = raw[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for (index, ch) in src.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn unsupported(what: &str) -> String {
    format!("{what} is not available in this embedded Python. {FIX}")
}
