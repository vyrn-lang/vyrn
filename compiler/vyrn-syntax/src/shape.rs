//! The tree a written parser builds: what type each rule answers with.
//!
//! A label is a field. A rule with no label is a choice of rule references when each
//! alternative holds one, and that reference when it holds one. Any other rule keeps
//! its span alone. A `climb` rule is an enum of its operand and a binary node. A
//! lexical rule's value is its text and its span.

use std::collections::BTreeSet;
use std::rc::Rc;

use crate::{Grammar, Refused, RuleId, E};

/// What one rule answers with.
#[derive(Debug, Clone)]
pub(crate) enum Shape {
    /// A lexical rule, which holds no tree.
    Lexical,
    /// A record of these fields, and its span.
    Record(Vec<Field>),
    /// An enum with one variant per alternative that can match; an alternative that
    /// only refuses has none.
    Choice(Vec<Option<String>>),
    /// The rule's one reference: no type of its own.
    Same(RuleId),
    /// An enum of this operand and a binary node.
    Climb(RuleId),
}

/// One field of a record.
#[derive(Debug, Clone)]
pub(crate) struct Field {
    pub(crate) name: Rc<str>,
    pub(crate) ty: Ty,
    pub(crate) mult: Mult,
}

/// What a label holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Ty {
    /// The text it matched, with its span.
    Text,
    Rule(RuleId),
}

/// How many values a label holds, ordered from fewest ways to hold none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mult {
    One,
    Opt,
    Many,
}

/// `file` as `File`.
pub(crate) fn type_name(rule: &str) -> String {
    let mut cs = rule.chars();
    match cs.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
        None => String::new(),
    }
}

/// The shape of each of `rules`, by rule index, or why a rule has no clear type. A
/// rule outside `rules` is [`Shape::Lexical`], and no written code reads it.
pub(crate) fn shapes<'a>(
    g: &Grammar,
    rules: &[RuleId],
    rows: impl Iterator<Item = &'a Rc<str>>,
) -> Result<Vec<Shape>, Refused> {
    let mut out = vec![Shape::Lexical; g.rules.len()];
    for &r in rules {
        let rule = g.body(r);
        if rule.syntactic {
            let no = |sentence: String| {
                Refused::at(rule.line, 1, format!("rule `{}`: {sentence}", rule.name))
            };
            out[r.0 as usize] = shape(g, r, &rule.body).map_err(no)?;
        }
    }
    refuse_endless(g, &out, rules)?;
    refuse_clashes(g, &out, rules, rows)?;
    Ok(out)
}

fn shape(g: &Grammar, r: RuleId, body: &E) -> Result<Shape, String> {
    if let E::Climb(operand, _) = body {
        return match &**operand {
            E::Ref(first) => Ok(Shape::Climb(*first)),
            _ => Err("the operand of a `climb` is one rule".into()),
        };
    }
    let mut fields: Vec<Field> = Vec::new();
    collect(body, Mult::One, &mut fields)?;
    if !fields.is_empty() {
        return refuse_bad_labels(fields).map(Shape::Record);
    }
    if let E::Alt(v) = body {
        let mut variants = Vec::new();
        for x in v {
            variants.push(match (refs(x), refuses(x)) {
                (Some(rs), _) if rs.len() == 1 => Some(format!(
                    "{}{}",
                    type_name(&g.body(r).name),
                    type_name(&g.body(rs[0]).name)
                )),
                (_, true) => None,
                (Some(rs), false) if rs.is_empty() => Some(String::new()),
                _ => return Err(format!("alternative {} keeps no one reference, so the rule has no clear type; give it a rule or a label", variants.len() + 1)),
            });
        }
        if variants.iter().flatten().all(|v| v.is_empty()) {
            return Ok(Shape::Record(Vec::new()));
        }
        if let Some(i) = variants.iter().position(|v| v.as_deref() == Some("")) {
            return Err(format!(
                "alternative {} keeps nothing and another keeps a rule, so the rule has no clear type; give it a rule of its own",
                i + 1
            ));
        }
        return Ok(Shape::Choice(variants));
    }
    Ok(match refs(body).as_deref() {
        Some([one]) => Shape::Same(*one),
        _ => Shape::Record(Vec::new()),
    })
}

/// The variant and rule of each alternative of a choice that has one.
pub(crate) fn variants<'a>(
    g: &'a Grammar,
    variants: &'a [Option<String>],
    r: RuleId,
) -> impl Iterator<Item = (&'a str, RuleId)> + 'a {
    let E::Alt(v) = &g.body(r).body else {
        unreachable!("a choice's body is a choice");
    };
    (v.iter().zip(variants)).filter_map(|(x, name)| {
        let name = name.as_deref()?;
        let rs = refs(x)?;
        Some((name, rs[0]))
    })
}

/// Adds the labels of `e` to `out`, each at the fewest values it can hold under `m`.
fn collect(e: &E, m: Mult, out: &mut Vec<Field>) -> Result<(), String> {
    match e {
        E::Field(name, x) => {
            let (ty, own) = label_value(x);
            add(
                out,
                Field {
                    name: name.clone(),
                    ty,
                    mult: own.max(m),
                },
            )?;
            nested(x, m, out)
        }
        E::Seq(v) => v.iter().try_for_each(|x| collect(x, m, out)),
        E::Alt(v) => {
            // A label in only some alternatives may hold nothing.
            let mut per: Vec<Vec<Field>> = Vec::new();
            for x in v {
                let mut one = Vec::new();
                collect(x, m, &mut one)?;
                per.push(one);
            }
            let mut merged: Vec<Field> = Vec::new();
            for one in &per {
                for f in one {
                    let everywhere = per.iter().all(|o| o.iter().any(|g| g.name == f.name));
                    let mult = if everywhere {
                        f.mult
                    } else {
                        f.mult.max(Mult::Opt)
                    };
                    match merged.iter_mut().find(|g| g.name == f.name) {
                        Some(g) if g.ty != f.ty => {
                            return Err(format!("label `{}` holds two types", f.name))
                        }
                        Some(g) => g.mult = g.mult.max(mult),
                        None => merged.push(Field { mult, ..f.clone() }),
                    }
                }
            }
            merged.into_iter().try_for_each(|f| add(out, f))
        }
        E::Star(x) | E::Plus(x) => collect(x, Mult::Many, out),
        E::Many(x, _, _) => collect(x, Mult::Many, out),
        E::List(x, sep, _) => {
            collect(x, Mult::Many, out)?;
            collect(sep, Mult::Many, out)
        }
        E::Opt(x) => collect(x, m.max(Mult::Opt), out),
        E::Try(x) | E::SetFlag(_, _, x) | E::Guard(_, x) => collect(x, m, out),
        _ => Ok(()),
    }
}

/// The labels under a label's own expression, past the forms [`label_value`] reads.
fn nested(x: &E, m: Mult, out: &mut Vec<Field>) -> Result<(), String> {
    match x {
        E::Star(y) | E::Plus(y) | E::Many(y, ..) | E::List(y, ..) => nested(y, Mult::Many, out),
        E::Opt(y) => nested(y, m.max(Mult::Opt), out),
        E::Try(y) | E::SetFlag(_, _, y) => nested(y, m, out),
        other => collect(other, m, out),
    }
}

/// What a label on `x` holds, and how many.
fn label_value(x: &E) -> (Ty, Mult) {
    match x {
        E::Ref(r) => (Ty::Rule(*r), Mult::One),
        E::Star(y) | E::Plus(y) | E::Many(y, ..) | E::List(y, ..) => (label_value(y).0, Mult::Many),
        E::Opt(y) => {
            let (ty, m) = label_value(y);
            (ty, m.max(Mult::Opt))
        }
        E::Try(y) | E::SetFlag(_, _, y) => label_value(y),
        _ => (Ty::Text, Mult::One),
    }
}

/// Adds a field. A label twice in one sequence holds an array.
fn add(out: &mut Vec<Field>, f: Field) -> Result<(), String> {
    match out.iter_mut().find(|g| g.name == f.name) {
        Some(g) if g.ty != f.ty => Err(format!("label `{}` holds two types", f.name)),
        Some(g) => {
            g.mult = Mult::Many;
            Ok(())
        }
        None => {
            out.push(f);
            Ok(())
        }
    }
}

/// Every word the language reserves, read from the line that states it.
fn reserved_words() -> Vec<&'static str> {
    (include_str!("../vyrn.syntax").lines())
        .filter_map(|l| l.strip_prefix("keyword "))
        .filter_map(|l| l.split_once('\''))
        .filter_map(|(_, w)| w.strip_suffix('\''))
        .collect()
}

/// Refuses a label that no field of a record can spell.
fn refuse_bad_labels(fields: Vec<Field>) -> Result<Vec<Field>, String> {
    let reserved = reserved_words();
    for f in &fields {
        if matches!(&*f.name, "start" | "end") {
            return Err(format!("label `{}` is a field of every record", f.name));
        }
        if reserved.contains(&&*f.name) {
            return Err(format!("label `{}` is a reserved word of Vyrn", f.name));
        }
    }
    Ok(fields)
}

/// The rules `e` always reads, once each, when it reads no rule in an optional or
/// repeated place; `None` otherwise. A predicate keeps nothing.
fn refs(e: &E) -> Option<Vec<RuleId>> {
    match e {
        E::Ref(r) => Some(vec![*r]),
        E::Seq(v) => {
            let mut out = Vec::new();
            for x in v {
                out.extend(refs(x)?);
            }
            Some(out)
        }
        E::Try(x) | E::SetFlag(_, _, x) | E::Guard(_, x) => refs(x),
        E::Star(x) | E::Plus(x) | E::Opt(x) => reads_none(x),
        E::Many(x, ..) => reads_none(x),
        E::List(x, sep, _) => {
            reads_none(x)?;
            reads_none(sep)
        }
        E::Alt(v) => {
            for x in v {
                reads_none(x)?;
            }
            Some(Vec::new())
        }
        _ => Some(Vec::new()),
    }
}

/// An empty list when `e` keeps no rule, and `None` when it does.
fn reads_none(e: &E) -> Option<Vec<RuleId>> {
    refs(e).filter(|rs| rs.is_empty())
}

/// Whether `e` can only refuse.
fn refuses(e: &E) -> bool {
    match e {
        E::Throw(_) => true,
        E::Seq(v) => v.iter().any(refuses),
        E::Alt(v) => v.iter().all(refuses),
        E::Guard(_, x) => refuses(x),
        _ => false,
    }
}

/// The rules whose types `r`'s type holds, past each rule that is its one reference.
fn holds(g: &Grammar, shapes: &[Shape], r: RuleId) -> Vec<RuleId> {
    let mut out = Vec::new();
    match &shapes[r.0 as usize] {
        Shape::Lexical => {}
        Shape::Record(fields) => {
            for f in fields {
                if let Ty::Rule(t) = f.ty {
                    out.push(t);
                }
            }
        }
        Shape::Choice(names) => out.extend(variants(g, names, r).map(|v| v.1)),
        Shape::Same(t) | Shape::Climb(t) => out.push(*t),
    }
    out
}

/// Whether `from`'s type holds `to`'s, walking only the rules `step` admits. Each
/// rule is entered once, so the walk ends.
fn reaches(
    g: &Grammar,
    shapes: &[Shape],
    from: RuleId,
    to: RuleId,
    step: &dyn Fn(&Shape) -> bool,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut todo = holds(g, shapes, from);
    while let Some(r) = todo.pop() {
        if r == to {
            return true;
        }
        if seen.insert(r.0) && step(&shapes[r.0 as usize]) {
            todo.extend(holds(g, shapes, r));
        }
    }
    false
}

/// Refuses a rule that reaches itself through its one reference alone: no type stands
/// on that cycle. Any other cycle holds a record or a choice, and [`recursive_records`]
/// and [`recursive_choices`] give it a release.
fn refuse_endless(g: &Grammar, shapes: &[Shape], rules: &[RuleId]) -> Result<(), Refused> {
    let same = |s: &Shape| matches!(s, Shape::Same(_));
    for &r in rules {
        if same(&shapes[r.0 as usize]) && reaches(g, shapes, r, r, &same) {
            let rule = g.body(r);
            let sentence = format!(
                "rule `{}` is its own one reference, so it has no type; give it a label",
                rule.name
            );
            return Err(Refused::at(rule.line, 1, sentence));
        }
    }
    Ok(())
}

/// Each record whose type reaches itself, with its fields: the type name and the
/// field names. A `climb` rule's binary node is such a record.
pub(crate) fn recursive_records(
    g: &Grammar,
    shapes: &[Shape],
    rules: &[RuleId],
) -> Vec<(String, Vec<Rc<str>>)> {
    let any = |_: &Shape| true;
    let mut out = Vec::new();
    for &r in rules {
        let name = type_name(&g.body(r).name);
        match &shapes[r.0 as usize] {
            Shape::Record(fields) if reaches(g, shapes, r, r, &any) => {
                out.push((name, fields.iter().map(|f| f.name.clone()).collect()));
            }
            Shape::Climb(_) => {
                let fields = ["left", "op", "right"].map(Rc::from).to_vec();
                out.push((format!("{name}Binary"), fields));
            }
            _ => {}
        }
    }
    out
}

/// Each choice that reaches itself with no record between, with its variants: the type
/// name and the variant names, each once. A record on a cycle ends that cycle's release.
pub(crate) fn recursive_choices(
    g: &Grammar,
    shapes: &[Shape],
    rules: &[RuleId],
) -> Vec<(String, Vec<String>)> {
    let no_record = |s: &Shape| matches!(s, Shape::Choice(_) | Shape::Same(_) | Shape::Climb(_));
    let mut out = Vec::new();
    for &r in rules {
        if let Shape::Choice(names) = &shapes[r.0 as usize] {
            if reaches(g, shapes, r, r, &no_record) {
                let mut arms: Vec<String> = Vec::new();
                for (v, _) in variants(g, names, r) {
                    if !arms.iter().any(|a| a == v) {
                        arms.push(v.to_string());
                    }
                }
                out.push((type_name(&g.body(r).name), arms));
            }
        }
    }
    out
}

/// The labels `e` binds outside a predicate.
pub(crate) fn labels_in(e: &E) -> Vec<Rc<str>> {
    let mut fields = Vec::new();
    // A label holding two types is refused before any rule is written.
    let _ = collect(e, Mult::One, &mut fields);
    fields.into_iter().map(|f| f.name).collect()
}

/// Refuses two written names that would clash: types, variants, rows and the names
/// the module uses for itself.
fn refuse_clashes<'a>(
    g: &Grammar,
    shapes: &[Shape],
    rules: &[RuleId],
    rows: impl Iterator<Item = &'a Rc<str>>,
) -> Result<(), Refused> {
    let mut names: BTreeSet<String> = rows.map(|r| r.to_string()).collect();
    names.extend(["Row", "Parse", "Lexeme"].map(String::from));
    for &r in rules {
        let rule = g.body(r);
        let ty = type_name(&rule.name);
        let mut written = Vec::new();
        match &shapes[r.0 as usize] {
            Shape::Record(_) => written.push(ty),
            Shape::Choice(variant_names) => {
                written.push(ty);
                let mut seen = BTreeSet::new();
                for (v, _) in variants(g, variant_names, r) {
                    if seen.insert(v) {
                        written.push(v.to_string());
                    }
                }
            }
            Shape::Climb(operand) => {
                written.push(format!("{ty}{}", type_name(&g.body(*operand).name)));
                written.push(format!("{ty}Op"));
                written.push(format!("{ty}Binary"));
                written.push(ty);
            }
            Shape::Same(_) | Shape::Lexical => {}
        }
        for name in written {
            if !names.insert(name.clone()) {
                let sentence = format!(
                    "rule `{}` writes the name `{name}`, and the module already has that name",
                    rule.name
                );
                return Err(Refused::at(rule.line, 1, sentence));
            }
        }
    }
    Ok(())
}
