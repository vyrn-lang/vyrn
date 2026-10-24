//! The orders of `std/order`: `Asc`, `Desc`, `Then`, `Elem`, the `Order`
//! bound and `before`, which the compiler reads by their `std/order`
//! declarations, never by spelling, so a program that does not import them
//! may declare its own.
//!
//! The loader records the linked names of those declarations ([`Names`]).
//! [`name`] then rewrites every written order into [`Type::Order`], its
//! normal form, and every `Elem<O>` into the element type, or into
//! [`Type::Elem`] where `O` is a type parameter. An [`Order`] is built only
//! by [`Order::asc`], [`Order::desc`] and [`Order::then`], so no other type
//! is an order.

use std::collections::HashMap;

use crate::ast::{Program, Type};
use crate::rules::{rule, Rule};
use crate::types::Decls;

/// The `std/order` module, as an import names it.
pub const SPEC: &str = "std/order";

/// The builtin `before`'s body calls; only `std/order` may call it
/// ([`crate::prelude::Builtin::home`]). The core lowers each call into the
/// order's key reads and compares.
pub const BEFORE_BUILTIN: &str = "orderBefore";

/// One of `std/order`'s compiler-known declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    Asc,
    Desc,
    Then,
    Elem,
    /// The protocol a type parameter takes to stand for an order.
    Order,
    /// `before<O>(a, b)`: `a` strictly before `b` under `O`.
    Before,
}

impl Known {
    fn of(written: &str) -> Option<Known> {
        Some(match written {
            "Asc" => Known::Asc,
            "Desc" => Known::Desc,
            "Then" => Known::Then,
            "Elem" => Known::Elem,
            "Order" => Known::Order,
            "before" => Known::Before,
            _ => return None,
        })
    }
}

/// The linked names of `std/order`'s compiler-known declarations in one
/// program. Empty where the program does not link `std/order`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names(Vec<(String, Known)>);

impl Names {
    /// The declarations of `program` that `std/order` makes, where `ours`
    /// answers whether a declaration's module is `std/order`.
    pub fn of(program: &Program, ours: &dyn Fn(&Option<String>) -> bool) -> Names {
        let known = |linked: &str| Known::of(program.spellings.written(linked));
        let types = (program.type_decls.iter())
            .filter(|d| ours(&d.module))
            .map(|d| &d.name);
        let protocols = (program.protocols.iter())
            .filter(|p| ours(&p.module))
            .map(|p| &p.name);
        let fns = (program.functions.iter())
            .filter(|f| ours(&f.module))
            .map(|f| &f.name);
        Names(
            types
                .chain(protocols)
                .chain(fns)
                .filter_map(|n| Some((n.clone(), known(n)?)))
                .collect(),
        )
    }

    /// What the declaration linked as `name` is, if `std/order` made it.
    pub fn known(&self, name: &str) -> Option<Known> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, k)| *k)
    }

    /// The linked name of `k`, where the program links it.
    pub fn name(&self, k: Known) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, x)| *x == k)
            .map(|(n, _)| n.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// An order in its normal form: the type it compares and its keys, most
/// significant first. Two orders with one normal form are one order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Order {
    pub(crate) elem: Type,
    keys: Vec<Key>,
}

/// One key of an [`Order`]: a field of the element, or the element itself,
/// and whether it runs descending.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub field: Option<String>,
    pub desc: bool,
}

impl Order {
    /// `Asc<elem, fields..>`: by each field in turn, or by the element's own
    /// order with no field. Each field is an integer, a `Bool` or a `String`
    /// field of the record `elem`, and with no field `elem` is one of those.
    pub fn asc(elem: Type, fields: &[String], decls: &dyn Decls) -> Result<Order, Rule> {
        let keys: Vec<Key> = match fields {
            [] => vec![Key {
                field: None,
                desc: false,
            }],
            fs => fs
                .iter()
                .map(|f| Key {
                    field: Some(f.clone()),
                    desc: false,
                })
                .collect(),
        };
        let o = Order { elem, keys }.normal();
        for k in &o.keys {
            o.key_type(k, decls)?;
        }
        Ok(o)
    }

    /// `Desc<o>`: every key reversed.
    pub fn desc(mut o: Order) -> Order {
        o.keys.iter_mut().for_each(|k| k.desc = !k.desc);
        o
    }

    /// `Then<a, b>`: `a`, then `b` among the values `a` ties. Both order one
    /// type.
    pub fn then(mut a: Order, b: Order) -> Result<Order, Rule> {
        if a.elem != b.elem {
            let (a, b) = (Type::Order(Box::new(a)), Type::Order(Box::new(b)));
            return Err(rule!(OrderThen, a, b));
        }
        a.keys.extend(b.keys);
        Ok(a.normal())
    }

    /// The type the order compares.
    pub fn elem(&self) -> &Type {
        &self.elem
    }

    /// The keys, most significant first.
    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// The type of key `k`'s value: the element's or its field's, resolved.
    pub fn key_type(&self, k: &Key, decls: &dyn Decls) -> Result<Type, Rule> {
        let order = Type::Order(Box::new(self.clone()));
        let ty = match &k.field {
            None => crate::types::resolve(&self.elem, decls),
            Some(f) => {
                let fields = crate::types::record_fields(&self.elem, decls);
                let found = fields
                    .as_deref()
                    .and_then(|fs| fs.iter().find(|x| x.name == *f));
                let Some(x) = found else {
                    let elem = self.elem.clone();
                    return Err(rule!(OrderField, order, field = f.as_str(), elem));
                };
                crate::types::resolve(&x.ty, decls)
            }
        };
        match (&ty, &k.field) {
            (Type::Int | Type::IntN { .. } | Type::Bool | Type::Str, _) => Ok(ty),
            (_, None) => Err(rule!(OrderElem, order, elem = self.elem.clone())),
            (_, Some(f)) => Err(rule!(OrderKey, order, field = f.as_str(), ty)),
        }
    }

    /// The order with each key after the first of its field dropped: a later
    /// key of a field an earlier key compares sees only ties.
    fn normal(mut self) -> Order {
        let mut seen: Vec<Option<String>> = Vec::new();
        self.keys.retain(|k| {
            let fresh = !seen.contains(&k.field);
            seen.push(k.field.clone());
            fresh
        });
        self
    }

    /// The order's canonical direction, and whether it is `self` reversed:
    /// the first key ascending. `a <= b` under `self` is `b <= a` under the
    /// reverse, so a comparison is stated once for both.
    pub fn canonical(&self) -> (Order, bool) {
        match self.keys.first().is_some_and(|k| k.desc) {
            true => (Order::desc(self.clone()), true),
            false => (self.clone(), false),
        }
    }

    /// Whether `self`'s keys start with all of `o`'s: a sequence sorted by
    /// `self` is sorted by `o`.
    pub fn refines(&self, o: &Order) -> bool {
        self.elem == o.elem && self.keys.starts_with(&o.keys)
    }

    /// Writes the order as `std/order` spells it: each run of keys of one
    /// direction an `Asc`, reversed by `Desc`, the runs joined by `Then`.
    pub(crate) fn write(
        &self,
        f: &mut std::fmt::Formatter<'_>,
        elem: &dyn std::fmt::Display,
    ) -> std::fmt::Result {
        let mut runs: Vec<(bool, Vec<&Key>)> = Vec::new();
        for k in &self.keys {
            match runs.last_mut() {
                Some((d, ks)) if *d == k.desc && k.field.is_some() => ks.push(k),
                _ => runs.push((k.desc, vec![k])),
            }
        }
        let n = runs.len();
        for (i, (desc, ks)) in runs.iter().enumerate() {
            if i + 1 < n {
                write!(f, "Then<")?;
            }
            if *desc {
                write!(f, "Desc<")?;
            }
            write!(f, "Asc<{elem}")?;
            for k in ks.iter().filter_map(|k| k.field.as_ref()) {
                write!(f, ", {k}")?;
            }
            write!(f, ">")?;
            if *desc {
                write!(f, ">")?;
            }
            if i + 1 < n {
                write!(f, ", ")?;
            }
        }
        (1..n).try_for_each(|_| write!(f, ">"))
    }
}

/// Rewrites every order and `Elem` that `program` writes, in declarations,
/// signatures, clauses and bodies, into [`Type::Order`] and the element
/// type, and an alias of an order into the order. Answers each refusal with
/// its line. Allocates nothing for a program that does not link
/// `std/order`.
pub fn name(program: &mut Program) -> Vec<(usize, Rule)> {
    if program.orders.is_empty() {
        return Vec::new();
    }
    let names = program.orders.clone();
    let decls = crate::types::decl_map(program);
    let mut aliases: HashMap<String, Type> = HashMap::new();
    let mut refused = Vec::new();
    // An alias may name another alias, so each round settles one more level;
    // the decls bound the rounds.
    for _ in 0..=program.type_decls.len() {
        let mut more = false;
        for d in &program.type_decls {
            if names.known(&d.name).is_some() || aliases.contains_key(&d.name) {
                continue;
            }
            if let Ok(Some(t)) = convert(&d.base, &names, &aliases, &decls) {
                aliases.insert(d.name.clone(), t);
                more = true;
            }
        }
        if !more {
            break;
        }
    }
    crate::ctor::each_written_type(program, &mut |t, line| match convert(
        t, &names, &aliases, &decls,
    ) {
        Ok(Some(o)) => *t = o,
        Ok(None) => {}
        Err(r) => {
            refused.push((line, r));
            *t = Type::Err;
        }
    });
    refused
}

/// `t` as an order or an element type, where it writes one: `None` where it
/// writes neither.
fn convert(
    t: &Type,
    names: &Names,
    aliases: &HashMap<String, Type>,
    decls: &dyn Decls,
) -> Result<Option<Type>, Rule> {
    let order = |t: &Type| -> Result<Order, Rule> {
        match convert(t, names, aliases, decls)? {
            Some(Type::Order(o)) => Ok(*o),
            _ => Err(rule!(NotAnOrder, ty = t.clone())),
        }
    };
    let (n, args) = match t {
        Type::Order(_) => return Ok(Some(t.clone())),
        Type::Named(n) => match aliases.get(n) {
            Some(a) => return Ok(Some(a.clone())),
            None => (n, &[][..]),
        },
        Type::App(n, args) => (n, args.as_slice()),
        _ => return Ok(None),
    };
    let Some(k) = names.known(n) else {
        return Ok(None);
    };
    let arity = |want: &'static str| rule!(OrderArity, name = crate::rules::DeclName(n), want);
    let o = match (k, args) {
        (Known::Asc, [elem, fields @ ..]) => {
            let fs = (fields.iter())
                .map(|f| match f {
                    Type::Named(f) | Type::Param(f) => Ok(f.clone()),
                    _ => Err(arity("a type and field names")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Order::asc(elem.clone(), &fs, decls)?
        }
        (Known::Desc, [o]) => Order::desc(order(o)?),
        (Known::Then, [a, b]) => Order::then(order(a)?, order(b)?)?,
        (Known::Elem, [Type::Param(p)]) => {
            return Ok(Some(Type::Elem(Box::new(Type::Param(p.clone())))))
        }
        (Known::Elem, [o]) => return Ok(Some(order(o)?.elem)),
        (Known::Asc, _) => return Err(arity("a type and field names")),
        (Known::Desc | Known::Elem, _) => return Err(arity("one order")),
        (Known::Then, _) => return Err(arity("two orders")),
        (Known::Order | Known::Before, _) => return Ok(None),
    };
    Ok(Some(Type::Order(Box::new(o))))
}
