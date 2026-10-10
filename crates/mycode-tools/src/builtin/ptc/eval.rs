//! Interpreter for one `run_code` program.
//!
//! Tool calls stay inside this future. The only value that leaves is the
//! program's `return` plus `console.log` lines.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value};
use tokio::task::JoinSet;

use super::parse::{BinOp, Expr, Program, Stmt, TemplatePart, Unary};
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

pub(super) async fn execute(
    program: &Program,
    catalog: &ToolCatalog,
    ctx: &ToolCtx,
    out: &ToolStream,
    limits: &Limits,
) -> Result<Outcome, Thrown> {
    let mut machine = Machine {
        catalog: catalog.clone(),
        ctx: ctx.clone(),
        out: out.clone(),
        limits: limits.clone(),
        env: Env::default(),
        logs: Vec::new(),
        steps: 0,
        depth: 0,
    };
    match machine.run(&program.stmts).await {
        Ok(Control::Return(value)) => Ok(Outcome {
            logs: machine.logs,
            value: Some(display(&value).map_err(thrown_from_stop)?),
        }),
        Ok(Control::Next) => Ok(Outcome {
            logs: machine.logs,
            value: None,
        }),
        Err(Stop::Throw(thrown) | Stop::Halt(thrown)) => Err(thrown),
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
}

impl Machine {
    async fn run(&mut self, stmts: &[Stmt]) -> Result<Control, Stop> {
        let mut last = Control::Next;
        for stmt in stmts {
            last = self.stmt(stmt).await?;
            if matches!(last, Control::Return(_)) {
                return Ok(last);
            }
        }
        Ok(last)
    }

    async fn stmt(&mut self, stmt: &Stmt) -> Result<Control, Stop> {
        self.bump()?;
        match stmt {
            Stmt::Let { name, value } => {
                let value = self.expr(value).await?;
                self.bind(name, value);
                Ok(Control::Next)
            }
            Stmt::Unpack { names, value } => {
                let value = self.expr(value).await?;
                let Some(items) = value.as_arr() else {
                    return Err(Stop::Throw(Thrown::script("unpack needs a list")));
                };
                if items.len() != names.len() {
                    return Err(Stop::Throw(Thrown::script(format!(
                        "expected {} values, got {}",
                        names.len(),
                        items.len()
                    ))));
                }
                for (name, item) in names.iter().zip(items) {
                    self.bind(name, item);
                }
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
            Stmt::ForOf { name, iter, body } => {
                let iter = self.expr(iter).await?;
                let items = iterate(&iter)?;
                for item in items {
                    self.bump()?;
                    self.bind(name, item);
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
            Expr::Call { callee, args } => {
                let callee = self.expr(callee).await?;
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.expr(arg).await?);
                }
                self.call(callee, values).await
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
        match op {
            BinOp::Add => add(&left, &right),
            BinOp::Sub => Ok(Val::Num(number(&left)? - number(&right)?)),
            BinOp::Mul => Ok(Val::Num(number(&left)? * number(&right)?)),
            BinOp::Div => {
                let divisor = number(&right)?;
                if divisor == 0.0 {
                    return Err(Stop::Throw(Thrown::script("division by zero")));
                }
                Ok(Val::Num(number(&left)? / divisor))
            }
            BinOp::Rem => {
                let divisor = number(&right)?;
                if divisor == 0.0 {
                    return Err(Stop::Throw(Thrown::script("division by zero")));
                }
                Ok(Val::Num(number(&left)? % divisor))
            }
            BinOp::Eq => Ok(Val::Bool(equals(&left, &right))),
            BinOp::Ne => Ok(Val::Bool(!equals(&left, &right))),
            BinOp::In => Ok(Val::Bool(contains(&right, &left)?)),
            BinOp::NotIn => Ok(Val::Bool(!contains(&right, &left)?)),
            BinOp::Lt => Ok(Val::Bool(compare(&left, &right)? < 0)),
            BinOp::Le => Ok(Val::Bool(compare(&left, &right)? <= 0)),
            BinOp::Gt => Ok(Val::Bool(compare(&left, &right)? > 0)),
            BinOp::Ge => Ok(Val::Bool(compare(&left, &right)? >= 0)),
            BinOp::And | BinOp::Or => unreachable!("short-circuit handled above"),
        }
    }

    async fn call(&mut self, callee: Val, args: Vec<Val>) -> Result<Val, Stop> {
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
                let mut parts = Vec::new();
                for arg in &args {
                    parts.push(display(arg)?);
                }
                if self.logs.len() < super::MAX_LOG_LINES {
                    self.logs.push(parts.join(" "));
                }
                Ok(Val::Null)
            }
            Val::ObjectKeys => {
                let Some(object) = args.first() else {
                    return Err(Stop::Throw(Thrown::script("Object.keys takes an object")));
                };
                let keys = object_keys(object)?;
                Ok(Val::arr(keys.into_iter().map(Val::Str).collect()))
            }
            Val::Method { recv, name } => self.method(recv.as_ref(), &name, &args).await,
            _ => Err(Stop::Throw(Thrown::script("value is not callable"))),
        }
    }

    async fn method(&mut self, recv: &Val, name: &str, args: &[Val]) -> Result<Val, Stop> {
        match (recv, name) {
            (Val::Str(text), "split") => {
                let sep = arg_str(args, 0, "split")?;
                let parts: Vec<Val> = if sep.is_empty() {
                    text.chars().map(|ch| Val::Str(ch.to_string())).collect()
                } else {
                    text.split(&sep)
                        .map(|part| Val::Str(part.to_owned()))
                        .collect()
                };
                Ok(Val::arr(parts))
            }
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

fn member(object: Val, name: &str) -> Result<Val, Stop> {
    match &object {
        Val::Tools => Ok(Val::ToolFn(name.to_owned())),
        Val::Console if name == "log" => Ok(Val::ConsoleLog),
        Val::ObjectCtor if name == "keys" => Ok(Val::ObjectKeys),
        Val::PromiseCtor if name == "all" => Ok(Val::PromiseAll),
        Val::Str(text) if name == "length" => Ok(Val::Num(text.chars().count() as f64)),
        Val::Arr(items) if name == "length" => Ok(Val::Num(lock_vec(items).len() as f64)),
        Val::Obj(map) => Ok(lock_map(map).get(name).cloned().unwrap_or(Val::Null)),
        Val::Str(_) | Val::Arr(_) => Ok(Val::Method {
            recv: Box::new(object),
            name: name.to_owned(),
        }),
        _ => Err(Stop::Throw(Thrown::script(format!(
            "cannot read property {name}"
        )))),
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
        _ => Err(Stop::Throw(Thrown::script("`in` needs a string or a list"))),
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

fn iterate(value: &Val) -> Result<Vec<Val>, Stop> {
    match value {
        Val::Arr(items) => Ok(lock_vec(items).clone()),
        Val::Str(text) => Ok(text.chars().map(|ch| Val::Str(ch.to_string())).collect()),
        _ => Err(Stop::Throw(Thrown::script(
            "for-of needs an array or string",
        ))),
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
