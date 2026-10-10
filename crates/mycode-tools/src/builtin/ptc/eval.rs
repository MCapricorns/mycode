//! Interpreter for one `run_code` program.
//!
//! Tool calls stay inside this future. The only value that leaves is the
//! program's `return` plus `print` lines.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value};
use tokio::task::JoinSet;

use super::parse::{BinOp, Expr, Program, Spanned, Stmt, TemplatePart, Unary};
use super::{Limits, Thrown, invoke_tool, is_concurrency_safe};
use crate::ctx::ToolCtx;
use crate::registry::ToolCatalog;
use crate::stream::ToolStream;

const MAX_STEPS: u32 = 20_000;
const MAX_DEPTH: u32 = 64;

pub(super) struct Outcome {
    pub logs: Vec<String>,
    pub value: Option<String>,
}

/// A program that stopped, plus anything already printed.
pub(super) struct ExecFailure {
    pub thrown: Thrown,
    pub logs: Vec<String>,
}

pub(super) async fn execute(
    program: &Program,
    catalog: &ToolCatalog,
    ctx: &ToolCtx,
    out: &ToolStream,
    limits: &Limits,
) -> Result<Outcome, ExecFailure> {
    let mut machine = Machine {
        catalog: catalog.clone(),
        ctx: ctx.clone(),
        out: out.clone(),
        limits: limits.clone(),
        env: Env::default(),
        logs: Vec::new(),
        steps: 0,
        depth: 0,
        at: None,
    };
    match machine.run(&program.stmts).await {
        Ok(Control::Return(value)) => {
            let logs = std::mem::take(&mut machine.logs);
            let shown = display(&value).map_err(|stop| ExecFailure {
                thrown: thrown_from_stop(stop),
                logs: logs.clone(),
            })?;
            Ok(Outcome {
                logs,
                value: Some(shown),
            })
        }
        Ok(Control::Next) => Ok(Outcome {
            logs: machine.logs,
            value: None,
        }),
        Err(Stop::Throw(thrown) | Stop::Halt(thrown)) => Err(ExecFailure {
            thrown,
            logs: machine.logs,
        }),
    }
}

fn thrown_from_stop(stop: Stop) -> Thrown {
    match stop {
        Stop::Throw(thrown) | Stop::Halt(thrown) => thrown,
    }
}

impl From<Thrown> for Stop {
    fn from(thrown: Thrown) -> Self {
        if thrown.catchable() {
            Self::Throw(thrown)
        } else {
            Self::Halt(thrown)
        }
    }
}

enum Control {
    /// A statement finished without returning.
    Next,
    Return(Val),
}

enum Stop {
    Throw(Thrown),
    Halt(Thrown),
}

struct Machine {
    catalog: ToolCatalog,
    ctx: ToolCtx,
    out: ToolStream,
    limits: Limits,
    env: Env,
    logs: Vec<String>,
    steps: u32,
    depth: u32,
    at: Option<(u32, String)>,
}

impl Machine {
    async fn run(&mut self, stmts: &[Spanned]) -> Result<Control, Stop> {
        let mut last = Control::Next;
        for stmt in stmts {
            last = self.stmt(stmt).await?;
            if matches!(last, Control::Return(_)) {
                return Ok(last);
            }
        }
        Ok(last)
    }

    async fn stmt(&mut self, spanned: &Spanned) -> Result<Control, Stop> {
        self.bump()?;
        self.at = Some((spanned.line, spanned.snippet.clone()));
        self.exec(&spanned.stmt)
            .await
            .map_err(|stop| self.locate(stop))
    }

    fn locate(&self, stop: Stop) -> Stop {
        let thrown = match &stop {
            Stop::Throw(thrown) | Stop::Halt(thrown) => thrown,
        };
        if thrown.message.starts_with("line ") {
            return stop;
        }
        let Some((line, snippet)) = &self.at else {
            return stop;
        };
        let message = format!("line {line}: `{snippet}` — {}", thrown.message);
        match stop {
            Stop::Throw(thrown) => Stop::Throw(Thrown { message, ..thrown }),
            Stop::Halt(thrown) => Stop::Halt(Thrown { message, ..thrown }),
        }
    }

    async fn exec(&mut self, stmt: &Stmt) -> Result<Control, Stop> {
        match stmt {
            Stmt::Let { name, value } => {
                let value = self.expr(value).await?;
                self.bind(name, value);
                Ok(Control::Next)
            }
            Stmt::Unpack { names, value } => {
                let value = self.expr(value).await?;
                self.bind_unpack(names, &value)?;
                Ok(Control::Next)
            }
            Stmt::SetIndex {
                object,
                index,
                value,
            } => {
                let object = self.expr(object).await?;
                let index = self.expr(index).await?;
                let value = self.expr(value).await?;
                assign_index(&object, &index, value)?;
                Ok(Control::Next)
            }
            Stmt::AugName { name, op, value } => {
                let current = self.lookup(name)?;
                let right = self.expr(value).await?;
                let next = self.apply_binop(*op, &current, &right)?;
                self.bind(name, next);
                Ok(Control::Next)
            }
            Stmt::AugIndex {
                object,
                index,
                op,
                value,
            } => {
                let object = self.expr(object).await?;
                let index = self.expr(index).await?;
                let current = index_with(object.clone(), &index)?;
                let right = self.expr(value).await?;
                let next = self.apply_binop(*op, &current, &right)?;
                assign_index(&object, &index, next)?;
                Ok(Control::Next)
            }
            Stmt::Return(expr) => {
                let value = match expr {
                    Some(expr) => self.expr(expr).await?,
                    None => Val::Null,
                };
                Ok(Control::Return(value))
            }
            Stmt::Throw(expr) => {
                let value = self.expr(expr).await?;
                Err(Stop::Throw(Thrown::script(display(&value)?)))
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let cond = self.expr(cond).await?;
                let body = if truthy(&cond) { then_body } else { else_body };
                Box::pin(self.run(body)).await
            }
            Stmt::While { cond, body } => loop {
                self.bump()?;
                let cond = self.expr(cond).await?;
                if !truthy(&cond) {
                    return Ok(Control::Next);
                }
                if let Control::Return(value) = Box::pin(self.run(body)).await? {
                    return Ok(Control::Return(value));
                }
            },
            Stmt::ForOf { names, iter, body } => {
                let iter = self.expr(iter).await?;
                let items = iterate(&iter)?;
                for item in items {
                    self.bump()?;
                    if names.len() == 1 {
                        self.bind(&names[0], item);
                    } else {
                        self.bind_unpack(names, &item)?;
                    }
                    if let Control::Return(value) = Box::pin(self.run(body)).await? {
                        return Ok(Control::Return(value));
                    }
                }
                Ok(Control::Next)
            }
            Stmt::Try {
                body,
                catch_name,
                catch_body,
            } => match Box::pin(self.run(body)).await {
                Ok(control) => Ok(control),
                Err(Stop::Throw(thrown)) => {
                    self.bind(catch_name, thrown_value(&thrown));
                    Box::pin(self.run(catch_body)).await
                }
                Err(halt) => Err(halt),
            },
            Stmt::Expr(expr) => {
                let _ = self.expr(expr).await?;
                Ok(Control::Next)
            }
        }
    }

    fn bind_unpack(&mut self, names: &[String], value: &Val) -> Result<(), Stop> {
        let Some(items) = value.as_arr() else {
            return Err(Stop::Throw(Thrown::script(
                "unpacking needs a list. `a, b = await gather(...)` returns one list; do not join it into a string first",
            )));
        };
        if items.len() != names.len() {
            return Err(Stop::Throw(Thrown::script(format!(
                "unpacking expected {} values, got {}. Check the list on the right",
                names.len(),
                items.len()
            ))));
        }
        for (name, item) in names.iter().zip(items) {
            self.bind(name, item);
        }
        Ok(())
    }

    fn bind(&mut self, name: &str, value: Val) {
        if self.env.get(name).is_some() {
            let _ = self.env.assign(name, value);
        } else {
            self.env.define(name, value);
        }
    }

    async fn expr(&mut self, expr: &Expr) -> Result<Val, Stop> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(Stop::Halt(Thrown::budget("expression is too deep")));
        }
        let result = Box::pin(self.expr_inner(expr)).await;
        self.depth -= 1;
        result
    }

    async fn expr_inner(&mut self, expr: &Expr) -> Result<Val, Stop> {
        match expr {
            Expr::Null => Ok(Val::Null),
            Expr::Bool(value) => Ok(Val::Bool(*value)),
            Expr::Num(value) => Ok(Val::Num(*value)),
            Expr::Str(value) => Ok(Val::Str(value.clone())),
            Expr::Ident(name) => self.lookup(name),
            Expr::Array(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.expr(item).await?);
                }
                Ok(Val::arr(values))
            }
            Expr::Object(fields) => {
                let mut map = BTreeMap::new();
                for (key, value) in fields {
                    map.insert(key.clone(), self.expr(value).await?);
                }
                Ok(Val::obj(map))
            }
            Expr::Template(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        TemplatePart::Lit(lit) => text.push_str(lit),
                        TemplatePart::Expr(expr) => {
                            text.push_str(&display(&self.expr(expr).await?)?)
                        }
                    }
                }
                Ok(Val::Str(text))
            }
            Expr::Unary { op, expr } => {
                let value = self.expr(expr).await?;
                match op {
                    Unary::Await => self.await_val(value).await,
                    Unary::Not => Ok(Val::Bool(!truthy(&value))),
                    Unary::Neg => Ok(Val::Num(-number(&value)?)),
                }
            }
            Expr::Binary { op, left, right } => self.binary(*op, left, right).await,
            Expr::Member { object, name } => {
                let object = self.expr(object).await?;
                member(object, name)
            }
            Expr::Index { object, index } => {
                let object = self.expr(object).await?;
                let index = self.expr(index).await?;
                index_with(object, &index)
            }
            Expr::Call {
                callee,
                args,
                kwargs,
            } => {
                let callee = self.expr(callee).await?;
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.expr(arg).await?);
                }
                let mut named = Vec::with_capacity(kwargs.len());
                for (key, expr) in kwargs {
                    named.push((key.clone(), self.expr(expr).await?));
                }
                self.call(callee, values, named).await
            }
            Expr::IfExp { test, body, orelse } => {
                let test = self.expr(test).await?;
                if truthy(&test) {
                    self.expr(body).await
                } else {
                    self.expr(orelse).await
                }
            }
            Expr::ListComp {
                target,
                iter,
                elt,
                ifs,
            } => self.list_comp(target, iter, elt, ifs).await,
        }
    }

    async fn list_comp(
        &mut self,
        target: &str,
        iter: &Expr,
        elt: &Expr,
        ifs: &[Expr],
    ) -> Result<Val, Stop> {
        let items = iterate(&self.expr(iter).await?)?;
        let mut out = Vec::new();
        for item in items {
            self.bump()?;
            self.env.push();
            self.env.define(target, item);
            let mut keep = true;
            for cond in ifs {
                if !truthy(&self.expr(cond).await?) {
                    keep = false;
                    break;
                }
            }
            if keep {
                out.push(self.expr(elt).await?);
            }
            self.env.pop();
        }
        Ok(Val::arr(out))
    }

    async fn binary(&mut self, op: BinOp, left: &Expr, right: &Expr) -> Result<Val, Stop> {
        if op == BinOp::And {
            let left = self.expr(left).await?;
            if !truthy(&left) {
                return Ok(left);
            }
            return self.expr(right).await;
        }
        if op == BinOp::Or {
            let left = self.expr(left).await?;
            if truthy(&left) {
                return Ok(left);
            }
            return self.expr(right).await;
        }
        let left = self.expr(left).await?;
        let right = self.expr(right).await?;
        self.apply_binop(op, &left, &right)
    }

    fn apply_binop(&mut self, op: BinOp, left: &Val, right: &Val) -> Result<Val, Stop> {
        match op {
            BinOp::Add => add(left, right),
            BinOp::Sub => Ok(Val::Num(number(left)? - number(right)?)),
            BinOp::Mul => multiply(left, right),
            BinOp::Div => {
                let divisor = number(right)?;
                if divisor == 0.0 {
                    return Err(Stop::Throw(Thrown::script("division by zero")));
                }
                Ok(Val::Num(number(left)? / divisor))
            }
            BinOp::Rem => {
                let divisor = number(right)?;
                if divisor == 0.0 {
                    return Err(Stop::Throw(Thrown::script("division by zero")));
                }
                Ok(Val::Num(number(left)? % divisor))
            }
            BinOp::Eq => Ok(Val::Bool(equals(left, right))),
            BinOp::Ne => Ok(Val::Bool(!equals(left, right))),
            BinOp::In => Ok(Val::Bool(contains(right, left)?)),
            BinOp::NotIn => Ok(Val::Bool(!contains(right, left)?)),
            BinOp::Lt => Ok(Val::Bool(compare(left, right)? < 0)),
            BinOp::Le => Ok(Val::Bool(compare(left, right)? <= 0)),
            BinOp::Gt => Ok(Val::Bool(compare(left, right)? > 0)),
            BinOp::Ge => Ok(Val::Bool(compare(left, right)? >= 0)),
            BinOp::And | BinOp::Or => unreachable!("short-circuit handled above"),
        }
    }

    async fn call(
        &mut self,
        callee: Val,
        args: Vec<Val>,
        kwargs: Vec<(String, Val)>,
    ) -> Result<Val, Stop> {
        if !matches!(callee, Val::DictFn) {
            expect_kwargs(&kwargs, allowed_keywords(&callee))?;
        }
        match callee {
            Val::ToolFn(name) => {
                let arg = match args.as_slice() {
                    [] => Val::obj(BTreeMap::new()),
                    [one] => one.clone(),
                    _ => {
                        return Err(Stop::Throw(Thrown::script(
                            "a tool call takes one object argument",
                        )));
                    }
                };
                if !matches!(arg, Val::Obj(_)) {
                    return Err(Stop::Throw(Thrown::script(
                        "a tool call takes one object argument",
                    )));
                }
                Ok(Val::Pending(Arc::new(Pending {
                    name,
                    args: arg,
                    gate: tokio::sync::Mutex::new(()),
                    result: Mutex::new(None),
                })))
            }
            Val::PromiseAll => {
                let list = if args.len() == 1 {
                    args.first()
                        .and_then(Val::as_arr)
                        .unwrap_or_else(|| args.clone())
                } else {
                    args
                };
                self.promise_all(list).await
            }
            Val::ConsoleLog => {
                let sep = match kw_value(&kwargs, "sep") {
                    Some(value) => display(&value)?,
                    None => " ".to_owned(),
                };
                let mut parts = Vec::new();
                for arg in &args {
                    parts.push(display(arg)?);
                }
                if self.logs.len() < super::MAX_LOG_LINES {
                    self.logs.push(parts.join(&sep));
                }
                Ok(Val::Null)
            }
            Val::Len => {
                let Some(value) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("len takes one value")));
                };
                let count = match value {
                    Val::Str(text) => text.chars().count(),
                    Val::Arr(items) => lock_vec(items).len(),
                    Val::Obj(map) => lock_map(map).len(),
                    _ => {
                        return Err(Stop::Throw(Thrown::script(
                            "len takes a string, list, or dict",
                        )));
                    }
                };
                Ok(Val::Num(count as f64))
            }
            Val::Range => range_list(&args),
            Val::Enumerate => enumerate_list(&args, &kwargs),
            Val::Zip => zip_lists(&args),
            Val::Sorted => sorted_list(&args, &kwargs),
            Val::Min => reduce_cmp(&args, true),
            Val::Max => reduce_cmp(&args, false),
            Val::Sum => sum_list(&args),
            Val::IntFn => int_value(&args),
            Val::ExceptionCtor => {
                let message = match args.first() {
                    Some(value) => display(value)?,
                    None => "Exception".to_owned(),
                };
                Ok(Val::Str(message))
            }
            Val::StrFn => {
                let Some(value) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("str takes one value")));
                };
                Ok(Val::Str(display(value)?))
            }
            Val::ObjectKeys => {
                let Some(object) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("Object.keys takes an object")));
                };
                let keys = object_keys(object)?;
                Ok(Val::arr(keys.into_iter().map(Val::Str).collect()))
            }
            Val::Method { recv, name } => self.method(recv.as_ref(), &name, &args, &kwargs).await,
            Val::ListFn | Val::TupleFn => list_value(&args),
            Val::DictFn => dict_value(&args, &kwargs),
            Val::SetFn => set_value(&args),
            Val::BoolFn => Ok(Val::Bool(args.first().is_some_and(truthy))),
            Val::FloatFn => float_value(&args),
            Val::AbsFn => abs_value(&args),
            Val::RoundFn => round_value(&args),
            Val::AnyFn => any_all(&args, false),
            Val::AllFn => any_all(&args, true),
            Val::ReversedFn => reversed_value(&args),
            Val::IsInstance => isinstance_value(&args),
            Val::ReprFn => {
                let Some(value) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("repr takes one value")));
                };
                Ok(Val::Str(repr_value(value)?))
            }
            _ => Err(Stop::Throw(Thrown::script("value is not callable"))),
        }
    }

    async fn method(
        &mut self,
        recv: &Val,
        name: &str,
        args: &[Val],
        kwargs: &[(String, Val)],
    ) -> Result<Val, Stop> {
        match (recv, name) {
            (Val::Str(text), "split") => {
                let parts: Vec<Val> = if args.is_empty() {
                    text.split_whitespace()
                        .map(|part| Val::Str(part.to_owned()))
                        .collect()
                } else {
                    let sep = arg_str(args, 0, "split")?;
                    if sep.is_empty() {
                        text.chars().map(|ch| Val::Str(ch.to_string())).collect()
                    } else {
                        text.split(&sep)
                            .map(|part| Val::Str(part.to_owned()))
                            .collect()
                    }
                };
                Ok(Val::arr(parts))
            }
            (Val::Str(text), "splitlines") => Ok(Val::arr(
                text.lines().map(|line| Val::Str(line.to_owned())).collect(),
            )),
            (Val::Str(text), "upper") => Ok(Val::Str(text.to_uppercase())),
            (Val::Str(text), "includes") => {
                Ok(Val::Bool(text.contains(&arg_str(args, 0, "includes")?)))
            }
            (Val::Str(text), "startsWith" | "startswith") => Ok(Val::Bool(
                text.starts_with(&arg_str(args, 0, "startswith")?),
            )),
            (Val::Str(text), "endsWith" | "endswith") => {
                Ok(Val::Bool(text.ends_with(&arg_str(args, 0, "endswith")?)))
            }
            (Val::Str(text), "trim" | "strip") => Ok(Val::Str(text.trim().to_owned())),
            (Val::Str(text), "toLowerCase" | "lower") => Ok(Val::Str(text.to_lowercase())),
            (Val::Str(sep), "join") => join_with(sep, args),
            (Val::Str(text), "replace") => {
                let old = arg_str(args, 0, "replace")?;
                let new = arg_str(args, 1, "replace")?;
                Ok(Val::Str(text.replace(&old, &new)))
            }
            (Val::Str(text), "count") => {
                let needle = arg_str(args, 0, "count")?;
                if needle.is_empty() {
                    return Ok(Val::Num((text.chars().count() + 1) as f64));
                }
                Ok(Val::Num(text.matches(needle.as_str()).count() as f64))
            }
            (Val::Str(text), "find") => {
                let needle = arg_str(args, 0, "find")?;
                let index = text
                    .find(needle.as_str())
                    .map(|byte| text[..byte].chars().count());
                Ok(Val::Num(index.map(|slot| slot as f64).unwrap_or(-1.0)))
            }
            (Val::Str(text), "slice") => Ok(Val::Str(slice_str(text, args)?)),
            (Val::Arr(items), "push" | "append") => {
                let mut borrowed = lock_vec(items);
                for arg in args {
                    borrowed.push(arg.clone());
                }
                let len = borrowed.len();
                Ok(Val::Num(len as f64))
            }
            (Val::Arr(items), "slice") => {
                let borrowed = lock_vec(items);
                Ok(Val::arr(slice_slice(&borrowed, args)?))
            }
            (Val::Arr(items), "join") => {
                let sep = args
                    .first()
                    .map(display)
                    .transpose()?
                    .unwrap_or_else(|| ",".to_owned());
                let borrowed = lock_vec(items);
                let mut parts = Vec::with_capacity(borrowed.len());
                for item in borrowed.iter() {
                    parts.push(display(item)?);
                }
                Ok(Val::Str(parts.join(&sep)))
            }
            (Val::Obj(map), "get") => {
                let Some(key) = args.first() else {
                    return Err(Stop::Throw(Thrown::script(
                        "dict.get takes a key, as in counts.get(name, 0)",
                    )));
                };
                let key = display(key)?;
                let fallback = args
                    .get(1)
                    .cloned()
                    .or_else(|| kw_value(kwargs, "default"))
                    .unwrap_or(Val::Null);
                Ok(lock_map(map).get(&key).cloned().unwrap_or(fallback))
            }
            (Val::Obj(map), "setdefault") => {
                let Some(key) = args.first() else {
                    return Err(Stop::Throw(Thrown::script(
                        "dict.setdefault takes a key, as in g.setdefault(\"k\", [])",
                    )));
                };
                let key = display(key)?;
                let fallback = args
                    .get(1)
                    .cloned()
                    .or_else(|| kw_value(kwargs, "default"))
                    .unwrap_or(Val::Null);
                let mut borrowed = lock_map(map);
                if !borrowed.contains_key(&key) {
                    borrowed.insert(key.clone(), fallback);
                }
                Ok(borrowed.get(&key).cloned().unwrap_or(Val::Null))
            }
            (Val::Obj(map), "update") => {
                if let Some(Val::Obj(other)) = args.first() {
                    let copied = lock_map(other).clone();
                    lock_map(map).extend(copied);
                } else if let Some(value) = args.first() {
                    for pair in iterate(value)? {
                        let Some(items) = pair.as_arr() else {
                            return Err(Stop::Throw(Thrown::script(
                                "dict.update takes a dict or a list of [key, value] pairs",
                            )));
                        };
                        if items.len() != 2 {
                            return Err(Stop::Throw(Thrown::script(
                                "dict.update pairs need two items",
                            )));
                        }
                        lock_map(map).insert(display(&items[0])?, items[1].clone());
                    }
                }
                Ok(Val::Null)
            }
            (Val::Obj(map), "keys") => Ok(Val::arr(
                lock_map(map).keys().cloned().map(Val::Str).collect(),
            )),
            (Val::Obj(map), "values") => Ok(Val::arr(lock_map(map).values().cloned().collect())),
            (Val::Obj(map), "items") => {
                let pairs = lock_map(map)
                    .iter()
                    .map(|(key, value)| Val::arr(vec![Val::Str(key.clone()), value.clone()]))
                    .collect();
                Ok(Val::arr(pairs))
            }
            (Val::Arr(items), "extend") => {
                let Some(value) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("list.extend takes an iterable")));
                };
                let extra = iterate(value)?;
                lock_vec(items).extend(extra);
                Ok(Val::Null)
            }
            (Val::Arr(items), "pop") => {
                let mut borrowed = lock_vec(items);
                if borrowed.is_empty() {
                    return Err(Stop::Throw(Thrown::script("pop from an empty list")));
                }
                let slot = match args.first() {
                    Some(Val::Num(number)) => {
                        index_at(*number, borrowed.len()).ok_or_else(|| {
                            Stop::Throw(Thrown::script("list.pop index is out of range"))
                        })?
                    }
                    Some(_) => {
                        return Err(Stop::Throw(Thrown::script(
                            "list.pop index must be a number",
                        )));
                    }
                    None => borrowed.len() - 1,
                };
                Ok(borrowed.remove(slot))
            }
            (Val::Arr(items), "insert") => {
                let Some(Val::Num(number)) = args.first() else {
                    return Err(Stop::Throw(Thrown::script(
                        "list.insert takes an index and a value",
                    )));
                };
                let Some(value) = args.get(1) else {
                    return Err(Stop::Throw(Thrown::script(
                        "list.insert takes an index and a value",
                    )));
                };
                let mut borrowed = lock_vec(items);
                let slot = clamp_index(*number, borrowed.len());
                borrowed.insert(slot, value.clone());
                Ok(Val::Null)
            }
            (Val::Arr(items), "index") => {
                let Some(needle) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("list.index takes a value")));
                };
                let borrowed = lock_vec(items);
                let slot = borrowed.iter().position(|item| equals(item, needle));
                match slot {
                    Some(slot) => Ok(Val::Num(slot as f64)),
                    None => Err(Stop::Throw(Thrown::script("value is not in the list"))),
                }
            }
            (Val::Arr(items), "sort") => {
                let mut borrowed = lock_vec(items);
                borrowed.sort_by(sort_pair);
                if kw_value(kwargs, "reverse").is_some_and(|value| truthy(&value)) {
                    borrowed.reverse();
                }
                Ok(Val::Null)
            }
            (Val::Arr(items), "includes") => {
                let needle = args.first().cloned().unwrap_or(Val::Null);
                let borrowed = lock_vec(items);
                Ok(Val::Bool(borrowed.iter().any(|item| equals(item, &needle))))
            }
            (Val::Arr(_), "map" | "filter") => Err(Stop::Throw(Thrown::script(
                "use a list comprehension instead of map or filter",
            ))),
            _ => Err(Stop::Throw(Thrown::script(format!(
                "unknown method {name}"
            )))),
        }
    }

    async fn await_val(&mut self, value: Val) -> Result<Val, Stop> {
        let Val::Pending(pending) = value else {
            return Ok(value);
        };
        let text = pending
            .run(&self.catalog, &self.ctx, &self.out, &self.limits)
            .await?;
        Ok(Val::Str(text))
    }

    async fn promise_all(&mut self, items: Vec<Val>) -> Result<Val, Stop> {
        let mut plans = Vec::new();
        for (index, item) in items.iter().enumerate() {
            if let Val::Pending(pending) = item {
                plans.push((index, Arc::clone(pending)));
            }
        }
        let mut finished = Vec::new();
        let mut cursor = 0;
        while cursor < plans.len() {
            self.bump()?;
            let safe = is_concurrency_safe(&plans[cursor].1.name);
            if !safe {
                let (index, pending) = &plans[cursor];
                let result = pending
                    .run(&self.catalog, &self.ctx, &self.out, &self.limits)
                    .await;
                finished.push((*index, result));
                cursor += 1;
                continue;
            }
            let mut end = cursor + 1;
            while end < plans.len()
                && end - cursor < super::MAX_PARALLEL
                && is_concurrency_safe(&plans[end].1.name)
            {
                end += 1;
            }
            let mut set = JoinSet::new();
            for (index, pending) in plans[cursor..end].iter().cloned() {
                let catalog = self.catalog.clone();
                let ctx = self.ctx.clone();
                let out = self.out.clone();
                let limits = self.limits.clone();
                set.spawn(async move {
                    let result = pending.run(&catalog, &ctx, &out, &limits).await;
                    (index, result)
                });
            }
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok(pair) => finished.push(pair),
                    Err(error) => {
                        return Err(Stop::Halt(Thrown::script(format!(
                            "tool task failed: {error}"
                        ))));
                    }
                }
            }
            cursor = end;
        }
        finished.sort_by_key(|(index, _)| *index);
        let mut values = items;
        for (index, result) in finished {
            values[index] = Val::Str(result?);
        }
        Ok(Val::arr(values))
    }

    fn lookup(&self, name: &str) -> Result<Val, Stop> {
        if let Some(value) = self.env.get(name) {
            return Ok(value);
        }
        match name {
            "tools" => Ok(Val::Tools),
            "gather" => Ok(Val::PromiseAll),
            "print" => Ok(Val::ConsoleLog),
            "len" => Ok(Val::Len),
            "range" => Ok(Val::Range),
            "str" => Ok(Val::StrFn),
            "int" => Ok(Val::IntFn),
            "enumerate" => Ok(Val::Enumerate),
            "zip" => Ok(Val::Zip),
            "sorted" => Ok(Val::Sorted),
            "min" => Ok(Val::Min),
            "max" => Ok(Val::Max),
            "sum" => Ok(Val::Sum),
            "Exception" => Ok(Val::ExceptionCtor),
            "list" => Ok(Val::ListFn),
            "tuple" => Ok(Val::TupleFn),
            "dict" => Ok(Val::DictFn),
            "set" => Ok(Val::SetFn),
            "bool" => Ok(Val::BoolFn),
            "float" => Ok(Val::FloatFn),
            "abs" => Ok(Val::AbsFn),
            "round" => Ok(Val::RoundFn),
            "any" => Ok(Val::AnyFn),
            "all" => Ok(Val::AllFn),
            "reversed" => Ok(Val::ReversedFn),
            "isinstance" => Ok(Val::IsInstance),
            "repr" => Ok(Val::ReprFn),
            "console" => Ok(Val::Console),
            "Object" => Ok(Val::ObjectCtor),
            "Promise" => Ok(Val::PromiseCtor),
            other => Err(Stop::Throw(Thrown::script(format!(
                "unknown variable {other}"
            )))),
        }
    }

    fn bump(&mut self) -> Result<(), Stop> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(Stop::Halt(Thrown::budget(format!(
                "program exceeded {MAX_STEPS} steps"
            ))));
        }
        self.limits.check().map_err(Stop::Halt)
    }
}

/// Methods `method()` implements. Attribute lookup must use this list so a
/// method cannot be implemented and then fail as "value is not callable".
pub(super) const IMPLEMENTED_METHODS: &[(&str, &[&str])] = &[
    (
        "str",
        &[
            "split",
            "splitlines",
            "upper",
            "includes",
            "startsWith",
            "startswith",
            "endsWith",
            "endswith",
            "trim",
            "strip",
            "toLowerCase",
            "lower",
            "join",
            "replace",
            "count",
            "find",
            "slice",
        ],
    ),
    (
        "list",
        &[
            "push", "append", "slice", "join", "extend", "pop", "insert", "index", "sort",
            "includes", "map", "filter",
        ],
    ),
    (
        "dict",
        &["get", "setdefault", "update", "keys", "values", "items"],
    ),
];

fn is_known_method(kind: &str, name: &str) -> bool {
    IMPLEMENTED_METHODS
        .iter()
        .any(|(candidate, names)| *candidate == kind && names.contains(&name))
}

#[cfg(test)]
pub(super) fn method_is_reachable(kind: &str, name: &str) -> bool {
    let object = match kind {
        "str" => Val::Str("ab".to_owned()),
        "list" => Val::arr(Vec::new()),
        "dict" => Val::obj(BTreeMap::new()),
        _ => return false,
    };
    matches!(member(object, name), Ok(Val::Method { .. }))
}

fn member(object: Val, name: &str) -> Result<Val, Stop> {
    match &object {
        Val::Tools => Ok(Val::ToolFn(name.to_owned())),
        Val::Console if name == "log" => Ok(Val::ConsoleLog),
        Val::ObjectCtor if name == "keys" => Ok(Val::ObjectKeys),
        Val::PromiseCtor if name == "all" => Ok(Val::PromiseAll),
        Val::Str(text) if name == "length" => Ok(Val::Num(text.chars().count() as f64)),
        Val::Arr(items) if name == "length" => Ok(Val::Num(lock_vec(items).len() as f64)),
        Val::Obj(_) if is_known_method("dict", name) => Ok(Val::Method {
            recv: Box::new(object.clone()),
            name: name.to_owned(),
        }),
        Val::Obj(map) => Ok(lock_map(map).get(name).cloned().unwrap_or(Val::Null)),
        Val::Str(_) if is_known_method("str", name) => Ok(Val::Method {
            recv: Box::new(object),
            name: name.to_owned(),
        }),
        Val::Arr(_) if is_known_method("list", name) => Ok(Val::Method {
            recv: Box::new(object),
            name: name.to_owned(),
        }),
        _ => Err(Stop::Throw(Thrown::script(format!("no attribute {name}")))),
    }
}

fn index_with(object: Val, index: &Val) -> Result<Val, Stop> {
    match (&object, index) {
        (Val::Tools, Val::Str(name)) => Ok(Val::ToolFn(name.clone())),
        (Val::Arr(items), Val::Num(number)) => {
            let items = lock_vec(items);
            let Some(slot) = index_at(*number, items.len()) else {
                return Ok(Val::Null);
            };
            Ok(items.get(slot).cloned().unwrap_or(Val::Null))
        }
        (Val::Str(text), Val::Num(number)) => {
            let chars: Vec<char> = text.chars().collect();
            let Some(slot) = index_at(*number, chars.len()) else {
                return Ok(Val::Null);
            };
            Ok(chars
                .get(slot)
                .map(|ch| Val::Str(ch.to_string()))
                .unwrap_or(Val::Null))
        }
        (Val::Obj(map), Val::Str(key)) => Ok(lock_map(map).get(key).cloned().unwrap_or(Val::Null)),
        _ => Err(Stop::Throw(Thrown::script("invalid index"))),
    }
}

struct Pending {
    name: String,
    args: Val,
    gate: tokio::sync::Mutex<()>,
    result: Mutex<Option<Result<String, Thrown>>>,
}

impl Pending {
    async fn run(
        &self,
        catalog: &ToolCatalog,
        ctx: &ToolCtx,
        out: &ToolStream,
        limits: &Limits,
    ) -> Result<String, Thrown> {
        let _gate = self.gate.lock().await;
        if let Some(done) = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
        {
            return done;
        }
        let args = val_to_json(&self.args).map_err(Thrown::script)?;
        let result = invoke_tool(catalog, ctx, out, limits, &self.name, args).await;
        *self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(result.clone());
        result
    }
}

#[derive(Clone)]
enum Val {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Arc<Mutex<Vec<Val>>>),
    Obj(Arc<Mutex<BTreeMap<String, Val>>>),
    Tools,
    Console,
    ObjectCtor,
    PromiseCtor,
    ToolFn(String),
    PromiseAll,
    ConsoleLog,
    Len,
    Range,
    StrFn,
    IntFn,
    Enumerate,
    Zip,
    Sorted,
    Min,
    Max,
    Sum,
    ExceptionCtor,
    ListFn,
    TupleFn,
    DictFn,
    SetFn,
    BoolFn,
    FloatFn,
    AbsFn,
    RoundFn,
    AnyFn,
    AllFn,
    ReversedFn,
    IsInstance,
    ReprFn,
    ObjectKeys,
    Pending(Arc<Pending>),
    Method { recv: Box<Val>, name: String },
}

impl Val {
    fn arr(items: Vec<Val>) -> Self {
        Self::Arr(Arc::new(Mutex::new(items)))
    }

    fn obj(fields: BTreeMap<String, Val>) -> Self {
        Self::Obj(Arc::new(Mutex::new(fields)))
    }

    fn as_arr(&self) -> Option<Vec<Val>> {
        match self {
            Self::Arr(items) => Some(lock_vec(items).clone()),
            _ => None,
        }
    }
}

fn lock_vec(items: &Mutex<Vec<Val>>) -> MutexGuard<'_, Vec<Val>> {
    items.lock().unwrap_or_else(|error| error.into_inner())
}

fn lock_map(items: &Mutex<BTreeMap<String, Val>>) -> MutexGuard<'_, BTreeMap<String, Val>> {
    items.lock().unwrap_or_else(|error| error.into_inner())
}

#[derive(Default)]
struct Env {
    frames: Vec<HashMap<String, Val>>,
}

impl Env {
    fn push(&mut self) {
        self.frames.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.frames.pop();
    }

    fn define(&mut self, name: &str, value: Val) {
        if self.frames.is_empty() {
            self.push();
        }
        self.frames
            .last_mut()
            .expect("frame")
            .insert(name.to_owned(), value);
    }

    fn assign(&mut self, name: &str, value: Val) -> Result<(), String> {
        for frame in self.frames.iter_mut().rev() {
            if frame.contains_key(name) {
                frame.insert(name.to_owned(), value);
                return Ok(());
            }
        }
        Err(format!("unknown variable {name}"))
    }

    fn get(&self, name: &str) -> Option<Val> {
        for frame in self.frames.iter().rev() {
            if let Some(value) = frame.get(name) {
                return Some(value.clone());
            }
        }
        None
    }
}

fn thrown_value(thrown: &Thrown) -> Val {
    let mut map = BTreeMap::new();
    if let Some(name) = &thrown.tool_name {
        map.insert("toolName".to_owned(), Val::Str(name.clone()));
    }
    map.insert("message".to_owned(), Val::Str(thrown.message.clone()));
    Val::obj(map)
}

fn truthy(value: &Val) -> bool {
    match value {
        Val::Null => false,
        Val::Bool(value) => *value,
        Val::Num(value) => *value != 0.0 && !value.is_nan(),
        Val::Str(value) => !value.is_empty(),
        Val::Arr(_) | Val::Obj(_) => true,
        _ => true,
    }
}

fn contains(haystack: &Val, needle: &Val) -> Result<bool, Stop> {
    match haystack {
        Val::Str(text) => {
            let Val::Str(needle) = needle else {
                return Err(Stop::Throw(Thrown::script(
                    "`in` on a string needs a string",
                )));
            };
            Ok(text.contains(needle))
        }
        Val::Arr(items) => Ok(lock_vec(items).iter().any(|item| equals(item, needle))),
        Val::Obj(map) => {
            let key = display(needle)?;
            Ok(lock_map(map).contains_key(&key))
        }
        _ => Err(Stop::Throw(Thrown::script(
            "`in` needs a string, a list, or a dict",
        ))),
    }
}

fn join_with(sep: &str, args: &[Val]) -> Result<Val, Stop> {
    let Some(items) = args.first().and_then(Val::as_arr) else {
        return Err(Stop::Throw(Thrown::script("join takes a list")));
    };
    let mut parts = Vec::with_capacity(items.len());
    for item in &items {
        parts.push(display(item)?);
    }
    Ok(Val::Str(parts.join(sep)))
}

fn number(value: &Val) -> Result<f64, Stop> {
    match value {
        Val::Num(value) => Ok(*value),
        _ => Err(Stop::Throw(Thrown::script("expected a number"))),
    }
}

fn equals(left: &Val, right: &Val) -> bool {
    match (left, right) {
        (Val::Null, Val::Null) => true,
        (Val::Bool(left), Val::Bool(right)) => left == right,
        (Val::Num(left), Val::Num(right)) => left == right,
        (Val::Str(left), Val::Str(right)) => left == right,
        (Val::Arr(left), Val::Arr(right)) if Arc::ptr_eq(left, right) => true,
        (Val::Arr(left), Val::Arr(right)) => {
            let left = lock_vec(left).clone();
            let right = lock_vec(right).clone();
            left.len() == right.len() && left.iter().zip(right.iter()).all(|(a, b)| equals(a, b))
        }
        (Val::Obj(left), Val::Obj(right)) if Arc::ptr_eq(left, right) => true,
        (Val::Obj(left), Val::Obj(right)) => {
            let left = lock_map(left).clone();
            let right = lock_map(right).clone();
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, value)| right.get(key).is_some_and(|other| equals(value, other)))
        }
        _ => false,
    }
}

fn compare(left: &Val, right: &Val) -> Result<i32, Stop> {
    match (left, right) {
        (Val::Num(left), Val::Num(right)) => Ok(ordering(
            left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal),
        )),
        (Val::Str(left), Val::Str(right)) => Ok(ordering(left.cmp(right))),
        _ => Err(Stop::Throw(Thrown::script(
            "comparison needs two numbers or two strings",
        ))),
    }
}

fn ordering(order: std::cmp::Ordering) -> i32 {
    match order {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn add(left: &Val, right: &Val) -> Result<Val, Stop> {
    if matches!(left, Val::Str(_)) || matches!(right, Val::Str(_)) {
        return Ok(Val::Str(format!("{}{}", display(left)?, display(right)?)));
    }
    Ok(Val::Num(number(left)? + number(right)?))
}

fn display(value: &Val) -> Result<String, Stop> {
    match value {
        Val::Null => Ok("null".to_owned()),
        Val::Bool(value) => Ok(value.to_string()),
        Val::Num(value) => Ok(format_num(*value)),
        Val::Str(value) => Ok(value.clone()),
        Val::Arr(_) | Val::Obj(_) => {
            let json =
                val_to_json(value).map_err(|message| Stop::Throw(Thrown::script(message)))?;
            Ok(json.to_string())
        }
        Val::Pending(_) => Err(Stop::Throw(Thrown::script("tool call was not awaited"))),
        Val::Method { .. } => Err(Stop::Throw(Thrown::script("function cannot be printed"))),
        _ => Err(Stop::Throw(Thrown::script("value cannot be printed"))),
    }
}

fn format_num(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

fn range_list(args: &[Val]) -> Result<Val, Stop> {
    let (start, end) = match args {
        [Val::Num(end)] => (0.0, *end),
        [Val::Num(start), Val::Num(end)] => (*start, *end),
        _ => {
            return Err(Stop::Throw(Thrown::script(
                "range takes one or two numbers",
            )));
        }
    };
    if !start.is_finite() || !end.is_finite() {
        return Err(Stop::Throw(Thrown::script("range bounds must be finite")));
    }
    let start = start.trunc() as i64;
    let end = end.trunc() as i64;
    if end <= start {
        return Ok(Val::arr(Vec::new()));
    }
    let span = end.checked_sub(start).unwrap_or(i64::MAX);
    const MAX_RANGE: i64 = 10_000;
    if span > MAX_RANGE {
        return Err(Stop::Throw(Thrown::script(format!(
            "range is limited to {MAX_RANGE} values"
        ))));
    }
    let mut items = Vec::with_capacity(span as usize);
    let mut cursor = start;
    while cursor < end {
        items.push(Val::Num(cursor as f64));
        cursor += 1;
    }
    Ok(Val::arr(items))
}

fn multiply(left: &Val, right: &Val) -> Result<Val, Stop> {
    match (left, right) {
        (Val::Str(text), Val::Num(times)) => repeat_str(text, *times),
        (Val::Num(times), Val::Str(text)) => repeat_str(text, *times),
        (Val::Arr(items), Val::Num(times)) => repeat_list(&lock_vec(items), *times),
        (Val::Num(times), Val::Arr(items)) => repeat_list(&lock_vec(items), *times),
        _ => Ok(Val::Num(number(left)? * number(right)?)),
    }
}

fn repeat_count(times: f64) -> Result<usize, Stop> {
    if !times.is_finite() {
        return Err(Stop::Throw(Thrown::script(
            "repeat count must be a finite number",
        )));
    }
    let raw = times.trunc() as i64;
    if raw <= 0 {
        return Ok(0);
    }
    Ok(raw as usize)
}

fn repeat_str(text: &str, times: f64) -> Result<Val, Stop> {
    let count = repeat_count(times)?;
    const MAX_REPEAT_CHARS: usize = 100_000;
    if text.chars().count().saturating_mul(count) > MAX_REPEAT_CHARS {
        return Err(Stop::Throw(Thrown::script(format!(
            "string repeat is limited to {MAX_REPEAT_CHARS} characters. Return a shorter string"
        ))));
    }
    Ok(Val::Str(text.repeat(count)))
}

fn repeat_list(items: &[Val], times: f64) -> Result<Val, Stop> {
    let count = repeat_count(times)?;
    const MAX_REPEAT_ITEMS: usize = 10_000;
    if items.len().saturating_mul(count) > MAX_REPEAT_ITEMS {
        return Err(Stop::Throw(Thrown::script(format!(
            "list repeat is limited to {MAX_REPEAT_ITEMS} items"
        ))));
    }
    let mut out = Vec::with_capacity(items.len().saturating_mul(count));
    for _ in 0..count {
        out.extend(items.iter().cloned());
    }
    Ok(Val::arr(out))
}

fn assign_index(object: &Val, index: &Val, value: Val) -> Result<(), Stop> {
    match object {
        Val::Obj(map) => {
            let key = display(index)?;
            lock_map(map).insert(key, value);
            Ok(())
        }
        Val::Arr(items) => {
            let Val::Num(number) = index else {
                return Err(Stop::Throw(Thrown::script(
                    "a list index assignment needs a number, as in items[i] = value",
                )));
            };
            let mut borrowed = lock_vec(items);
            let Some(slot) = index_at(*number, borrowed.len()) else {
                return Err(Stop::Throw(Thrown::script(
                    "list index is out of range. Assign an index that already exists",
                )));
            };
            borrowed[slot] = value;
            Ok(())
        }
        _ => Err(Stop::Throw(Thrown::script(
            "only a list or a dict can be assigned by index",
        ))),
    }
}

fn enumerate_list(args: &[Val], kwargs: &[(String, Val)]) -> Result<Val, Stop> {
    let Some(iter) = args.first() else {
        return Err(Stop::Throw(Thrown::script(
            "enumerate takes a list or a string",
        )));
    };
    if args.len() > 2 {
        return Err(Stop::Throw(Thrown::script(
            "enumerate takes the iterable and an optional start",
        )));
    }
    let mut start = match args.get(1) {
        Some(Val::Num(number)) => *number,
        Some(_) => {
            return Err(Stop::Throw(Thrown::script(
                "enumerate start must be a number",
            )));
        }
        None => 0.0,
    };
    for (key, value) in kwargs {
        if key != "start" {
            return Err(Stop::Throw(Thrown::script(format!(
                "enumerate accepts start=, not {key}"
            ))));
        }
        if args.len() > 1 {
            return Err(Stop::Throw(Thrown::script(
                "pass enumerate's start once, either as the second argument or as start=",
            )));
        }
        let Val::Num(number) = value else {
            return Err(Stop::Throw(Thrown::script(
                "enumerate start must be a number",
            )));
        };
        start = *number;
    }
    if !start.is_finite() {
        return Err(Stop::Throw(Thrown::script(
            "enumerate start must be finite",
        )));
    }
    let origin = start.trunc() as i64;
    let pairs = iterate(iter)?
        .into_iter()
        .enumerate()
        .map(|(index, item)| Val::arr(vec![Val::Num((origin + index as i64) as f64), item]))
        .collect();
    Ok(Val::arr(pairs))
}

fn zip_lists(args: &[Val]) -> Result<Val, Stop> {
    if args.is_empty() {
        return Ok(Val::arr(Vec::new()));
    }
    let lists = args.iter().map(iterate).collect::<Result<Vec<_>, _>>()?;
    let len = lists.iter().map(Vec::len).min().unwrap_or(0);
    let mut rows = Vec::with_capacity(len);
    for index in 0..len {
        rows.push(Val::arr(
            lists.iter().map(|list| list[index].clone()).collect(),
        ));
    }
    Ok(Val::arr(rows))
}

fn sorted_list(args: &[Val], kwargs: &[(String, Val)]) -> Result<Val, Stop> {
    let Some(iter) = args.first() else {
        return Err(Stop::Throw(Thrown::script(
            "sorted takes one list or string",
        )));
    };
    if args.len() != 1 {
        return Err(Stop::Throw(Thrown::script(
            "sorted takes one list or string",
        )));
    }
    let mut reverse = false;
    for (key, value) in kwargs {
        if key != "reverse" {
            return Err(Stop::Throw(Thrown::script(format!(
                "sorted accepts reverse=True or reverse=False. key= is not available because lambda is not available. Not {key}"
            ))));
        }
        reverse = truthy(value);
    }
    let mut items = iterate(iter)?;
    items.sort_by(sort_pair);
    if reverse {
        items.reverse();
    }
    Ok(Val::arr(items))
}

fn sort_pair(left: &Val, right: &Val) -> std::cmp::Ordering {
    match (left, right) {
        (Val::Num(left), Val::Num(right)) => {
            left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
        }
        (Val::Str(left), Val::Str(right)) => left.cmp(right),
        _ => display(left)
            .unwrap_or_default()
            .cmp(&display(right).unwrap_or_default()),
    }
}

fn reduce_cmp(args: &[Val], want_min: bool) -> Result<Val, Stop> {
    let name = if want_min { "min" } else { "max" };
    let items = if args.len() == 1 {
        iterate(&args[0])?
    } else if args.is_empty() {
        return Err(Stop::Throw(Thrown::script(format!(
            "{name} takes a list or several values"
        ))));
    } else {
        args.to_vec()
    };
    let mut items = items.into_iter();
    let Some(mut best) = items.next() else {
        return Err(Stop::Throw(Thrown::script(format!(
            "{name} of an empty list"
        ))));
    };
    for item in items {
        let order = sort_pair(&best, &item);
        let replace = if want_min {
            order == std::cmp::Ordering::Greater
        } else {
            order == std::cmp::Ordering::Less
        };
        if replace {
            best = item;
        }
    }
    Ok(best)
}

fn sum_list(args: &[Val]) -> Result<Val, Stop> {
    let Some(iter) = args.first() else {
        return Err(Stop::Throw(Thrown::script("sum takes a list of numbers")));
    };
    if args.len() > 2 {
        return Err(Stop::Throw(Thrown::script(
            "sum takes a list and an optional start",
        )));
    }
    let mut total = match args.get(1) {
        Some(Val::Num(number)) => *number,
        Some(_) => {
            return Err(Stop::Throw(Thrown::script("sum start must be a number")));
        }
        None => 0.0,
    };
    for item in iterate(iter)? {
        total += number(&item)?;
    }
    Ok(Val::Num(total))
}

fn int_value(args: &[Val]) -> Result<Val, Stop> {
    let Some(value) = args.first() else {
        return Err(Stop::Throw(Thrown::script("int takes one value")));
    };
    if args.len() != 1 {
        return Err(Stop::Throw(Thrown::script("int takes one value")));
    }
    let number = match value {
        Val::Num(number) => *number,
        Val::Bool(true) => 1.0,
        Val::Bool(false) => 0.0,
        Val::Str(text) => text.trim().parse::<f64>().map_err(|_| {
            Stop::Throw(Thrown::script(format!(
                "int cannot parse {text:?}. Pass a number or a numeric string"
            )))
        })?,
        _ => {
            return Err(Stop::Throw(Thrown::script(
                "int takes a number, a bool, or a numeric string",
            )));
        }
    };
    if !number.is_finite() {
        return Err(Stop::Throw(Thrown::script("int argument must be finite")));
    }
    Ok(Val::Num(number.trunc()))
}

fn kw_value(kwargs: &[(String, Val)], name: &str) -> Option<Val> {
    kwargs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn list_value(args: &[Val]) -> Result<Val, Stop> {
    match args {
        [] => Ok(Val::arr(Vec::new())),
        [one] => Ok(Val::arr(iterate(one)?)),
        _ => Err(Stop::Throw(Thrown::script("list takes one iterable"))),
    }
}

fn dict_value(args: &[Val], kwargs: &[(String, Val)]) -> Result<Val, Stop> {
    let mut fields = match args {
        [] => BTreeMap::new(),
        [Val::Obj(map)] => lock_map(map).clone(),
        [one] => pairs_to_dict(one)?,
        _ => {
            return Err(Stop::Throw(Thrown::script(
                "dict takes one mapping and keyword arguments, as in dict(pairs, a=1)",
            )));
        }
    };
    for (key, value) in kwargs {
        fields.insert(key.clone(), value.clone());
    }
    Ok(Val::obj(fields))
}

fn pairs_to_dict(value: &Val) -> Result<BTreeMap<String, Val>, Stop> {
    let mut fields = BTreeMap::new();
    for pair in iterate(value)? {
        let Some(items) = pair.as_arr() else {
            return Err(Stop::Throw(Thrown::script(
                "dict takes a dict or a list of [key, value] pairs",
            )));
        };
        if items.len() != 2 {
            return Err(Stop::Throw(Thrown::script("dict pairs need two items")));
        }
        fields.insert(display(&items[0])?, items[1].clone());
    }
    Ok(fields)
}

fn allowed_keywords(callee: &Val) -> &'static [&'static str] {
    match callee {
        Val::Enumerate => &["start"],
        Val::Sorted => &["reverse"],
        Val::ConsoleLog => &["sep"],
        Val::Method { name, .. } => match name.as_str() {
            "get" | "setdefault" => &["default"],
            "sort" => &["reverse"],
            _ => &[],
        },
        _ => &[],
    }
}

fn expect_kwargs(kwargs: &[(String, Val)], allowed: &[&str]) -> Result<(), Stop> {
    for (key, _) in kwargs {
        if !allowed.contains(&key.as_str()) {
            let accepted = if allowed.is_empty() {
                "no keyword arguments".to_owned()
            } else {
                allowed.join(", ")
            };
            return Err(Stop::Throw(Thrown::script(format!(
                "unexpected keyword argument {key}. This call accepts {accepted}"
            ))));
        }
    }
    Ok(())
}

fn set_value(args: &[Val]) -> Result<Val, Stop> {
    let items = match args {
        [] => Vec::new(),
        [one] => iterate(one)?,
        _ => {
            return Err(Stop::Throw(Thrown::script("set takes one iterable")));
        }
    };
    let mut unique = Vec::new();
    for item in items {
        if !unique.iter().any(|have| equals(have, &item)) {
            unique.push(item);
        }
    }
    Ok(Val::arr(unique))
}

fn float_value(args: &[Val]) -> Result<Val, Stop> {
    let Some(value) = args.first() else {
        return Err(Stop::Throw(Thrown::script("float takes one value")));
    };
    let number = match value {
        Val::Num(number) => *number,
        Val::Bool(true) => 1.0,
        Val::Bool(false) => 0.0,
        Val::Str(text) => text.trim().parse::<f64>().map_err(|_| {
            Stop::Throw(Thrown::script(format!(
                "float cannot parse {text:?}. Pass a number or a numeric string"
            )))
        })?,
        _ => {
            return Err(Stop::Throw(Thrown::script(
                "float takes a number or a numeric string",
            )));
        }
    };
    if !number.is_finite() {
        return Err(Stop::Throw(Thrown::script("float argument must be finite")));
    }
    Ok(Val::Num(number))
}

fn abs_value(args: &[Val]) -> Result<Val, Stop> {
    let Some(value) = args.first() else {
        return Err(Stop::Throw(Thrown::script("abs takes one number")));
    };
    Ok(Val::Num(number(value)?.abs()))
}

fn round_value(args: &[Val]) -> Result<Val, Stop> {
    let Some(value) = args.first() else {
        return Err(Stop::Throw(Thrown::script("round takes a number")));
    };
    let number = number(value)?;
    let digits = match args.get(1) {
        Some(Val::Num(digits)) => digits.trunc() as i32,
        None => 0,
        Some(_) => {
            return Err(Stop::Throw(Thrown::script("round digits must be a number")));
        }
    };
    if digits <= 0 {
        return Ok(Val::Num(number.round()));
    }
    let scale = 10_f64.powi(digits.min(8));
    Ok(Val::Num((number * scale).round() / scale))
}

fn any_all(args: &[Val], want_all: bool) -> Result<Val, Stop> {
    let name = if want_all { "all" } else { "any" };
    let Some(value) = args.first() else {
        return Err(Stop::Throw(Thrown::script(format!(
            "{name} takes an iterable"
        ))));
    };
    let items = iterate(value)?;
    let flag = if want_all {
        items.iter().all(truthy)
    } else {
        items.iter().any(truthy)
    };
    Ok(Val::Bool(flag))
}

fn reversed_value(args: &[Val]) -> Result<Val, Stop> {
    let Some(value) = args.first() else {
        return Err(Stop::Throw(Thrown::script("reversed takes an iterable")));
    };
    let mut items = iterate(value)?;
    items.reverse();
    Ok(Val::arr(items))
}

fn isinstance_value(args: &[Val]) -> Result<Val, Stop> {
    let (Some(value), Some(kind)) = (args.first(), args.get(1)) else {
        return Err(Stop::Throw(Thrown::script(
            "isinstance takes a value and a type, as in isinstance(name, str)",
        )));
    };
    Ok(Val::Bool(type_accepts(value, kind)))
}

fn type_accepts(value: &Val, kind: &Val) -> bool {
    match kind {
        Val::Arr(items) => lock_vec(items).iter().any(|item| type_accepts(value, item)),
        Val::StrFn => matches!(value, Val::Str(_)),
        Val::IntFn => matches!(value, Val::Num(_) | Val::Bool(_)),
        Val::FloatFn => matches!(value, Val::Num(_)),
        Val::BoolFn => matches!(value, Val::Bool(_)),
        Val::ListFn | Val::TupleFn | Val::SetFn => matches!(value, Val::Arr(_)),
        Val::DictFn => matches!(value, Val::Obj(_)),
        Val::Str(name) => type_name(value, name),
        _ => false,
    }
}

fn type_name(value: &Val, name: &str) -> bool {
    match name {
        "str" => matches!(value, Val::Str(_)),
        "int" => matches!(value, Val::Num(_) | Val::Bool(_)),
        "float" => matches!(value, Val::Num(_)),
        "bool" => matches!(value, Val::Bool(_)),
        "list" | "tuple" | "set" => matches!(value, Val::Arr(_)),
        "dict" => matches!(value, Val::Obj(_)),
        _ => false,
    }
}

fn repr_value(value: &Val) -> Result<String, Stop> {
    match value {
        Val::Null => Ok("None".to_owned()),
        Val::Bool(true) => Ok("True".to_owned()),
        Val::Bool(false) => Ok("False".to_owned()),
        Val::Num(number) => Ok(format_num(*number)),
        Val::Str(text) => Ok(format!("{text:?}")),
        Val::Arr(items) => {
            let mut parts = Vec::new();
            for item in lock_vec(items).iter() {
                parts.push(repr_value(item)?);
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        Val::Obj(map) => {
            let mut parts = Vec::new();
            for (key, item) in lock_map(map).iter() {
                parts.push(format!("{:?}: {}", key, repr_value(item)?));
            }
            Ok(format!("{{{}}}", parts.join(", ")))
        }
        _ => display(value),
    }
}

fn iterate(value: &Val) -> Result<Vec<Val>, Stop> {
    match value {
        Val::Arr(items) => Ok(lock_vec(items).clone()),
        Val::Str(text) => Ok(text.chars().map(|ch| Val::Str(ch.to_string())).collect()),
        _ => Err(Stop::Throw(Thrown::script("for needs a list or a string"))),
    }
}

fn arg_str(args: &[Val], index: usize, method: &str) -> Result<String, Stop> {
    match args.get(index) {
        Some(Val::Str(text)) => Ok(text.clone()),
        _ => Err(Stop::Throw(Thrown::script(format!(
            "{method} takes a string"
        )))),
    }
}

fn slice_bounds(len: usize, args: &[Val]) -> Result<(usize, usize), Stop> {
    let start = match args.first() {
        Some(Val::Num(value)) => clamp_index(*value, len),
        _ => {
            return Err(Stop::Throw(Thrown::script("slice start must be a number")));
        }
    };
    let end = match args.get(1) {
        Some(Val::Num(value)) => clamp_index(*value, len),
        None => len,
        _ => return Err(Stop::Throw(Thrown::script("slice end must be a number"))),
    };
    Ok((start, end))
}

fn clamp_index(value: f64, len: usize) -> usize {
    if !value.is_finite() {
        return 0;
    }
    let len = len as i64;
    let raw = value.trunc() as i64;
    let shifted = if raw < 0 { len + raw } else { raw };
    shifted.clamp(0, len) as usize
}

fn index_at(value: f64, len: usize) -> Option<usize> {
    if !value.is_finite() {
        return None;
    }
    let raw = value.trunc() as i64;
    if raw < 0 {
        let shifted = len as i64 + raw;
        (shifted >= 0).then_some(shifted as usize)
    } else {
        let slot = raw as usize;
        (slot < len).then_some(slot)
    }
}

fn slice_str(text: &str, args: &[Val]) -> Result<String, Stop> {
    let chars: Vec<char> = text.chars().collect();
    let (start, end) = slice_bounds(chars.len(), args)?;
    if start >= end {
        return Ok(String::new());
    }
    Ok(chars[start..end].iter().collect())
}

fn slice_slice(items: &[Val], args: &[Val]) -> Result<Vec<Val>, Stop> {
    let (start, end) = slice_bounds(items.len(), args)?;
    if start >= end {
        return Ok(Vec::new());
    }
    Ok(items[start..end].to_vec())
}

fn object_keys(value: &Val) -> Result<Vec<String>, Stop> {
    match value {
        Val::Obj(map) => Ok(lock_map(map).keys().cloned().collect()),
        Val::Arr(items) => Ok((0..lock_vec(items).len())
            .map(|index| index.to_string())
            .collect()),
        _ => Err(Stop::Throw(Thrown::script("Object.keys takes an object"))),
    }
}

fn val_to_json(value: &Val) -> Result<Value, String> {
    match value {
        Val::Null => Ok(Value::Null),
        Val::Bool(value) => Ok(Value::Bool(*value)),
        Val::Num(value) => {
            if value.fract() == 0.0 && value.abs() < 1e15 {
                Ok(Value::from(*value as i64))
            } else if value.is_finite() {
                Ok(Value::from(*value))
            } else {
                Err("number is not finite".to_owned())
            }
        }
        Val::Str(value) => Ok(Value::String(value.clone())),
        Val::Arr(items) => {
            let snapshot = lock_vec(items).clone();
            let mut out = Vec::new();
            for item in &snapshot {
                out.push(val_to_json(item)?);
            }
            Ok(Value::Array(out))
        }
        Val::Obj(map) => {
            let snapshot = lock_map(map).clone();
            let mut out = Map::new();
            for (key, item) in &snapshot {
                out.insert(key.clone(), val_to_json(item)?);
            }
            Ok(Value::Object(out))
        }
        _ => Err("tool arguments must be JSON".to_owned()),
    }
}
