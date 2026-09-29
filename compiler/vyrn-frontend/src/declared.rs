//! The program-level tables the ownership passes read: for a type, whether it
//! owns heap, how it is released and whether it must be used ([`Owned`]); for
//! a program, its declarations, parameter types, constructors and capabilities
//! ([`Declared`], [`arg_caps`]). Each reads a declaration; the type of an
//! expression is the checker's record ([`Declared::type_of`]). `None` means
//! "do not release" and "does not move", so an unnamed type leaks, which is
//! safe.

use std::collections::HashMap;

use crate::ast::*;
use crate::own::{DropKind, Linear};

/// The `Owned` protocol: how a type is released, answered only here. The
/// built-in rows are seeded, not read from `std/`, because a bare file has no
/// std; a program adds rows with `impl Owned for T`. `Default` is the seed
/// alone.
#[derive(Clone, Default)]
pub struct Owned {
    /// Type key to its flattened `release`, one per `impl Owned for T`.
    impls: HashMap<String, String>,
    /// Type keys with `impl MustUse for T`.
    linear: std::collections::HashSet<String>,
    types: HashMap<String, TypeDecl>,
    /// Whether a type parameter answers as a String does ([`Owned::as_written`]).
    params_own: bool,
}

impl Owned {
    pub fn new(program: &Program) -> Self {
        let impls = program
            .impls
            .iter()
            .filter(|i| i.protocol == crate::types::OWNED)
            // A generic head is keyed by its type constructor (`Slots`); each
            // drop site solves the instance from the binding's type.
            .filter_map(|i| crate::types::type_key(&i.ty))
            .map(|k| {
                let m = crate::types::impl_method_name(
                    crate::types::OWNED,
                    &k,
                    crate::types::OWNED_RELEASE,
                );
                (k, m)
            })
            .collect();
        Owned {
            impls,
            // Keyed by constructor, so `impl<T> MustUse for Pool<T>` obliges
            // every instance.
            linear: program
                .impls
                .iter()
                .filter(|i| i.protocol == crate::types::MUST_USE)
                .filter_map(|i| crate::types::type_key(&i.ty))
                .collect(),
            types: crate::types::decl_map(program),
            params_own: false,
        }
    }

    /// Returns this table with every type parameter owning heap, for judging a
    /// generic body as written: it must hold at the worst instance, and no user
    /// bound can say `@Heapless`.
    pub fn as_written(&self) -> Self {
        Owned {
            params_own: true,
            ..self.clone()
        }
    }

    /// `ty` with every type parameter read as `String` when `params_own` is set.
    fn standing<'t>(&self, ty: &'t Type) -> std::borrow::Cow<'t, Type> {
        if !self.params_own || !crate::types::mentions_param(ty) {
            return std::borrow::Cow::Borrowed(ty);
        }
        let mut subst = HashMap::new();
        crate::types::walk_type(ty, &mut |t| {
            if let Type::Param(p) = t {
                subst.insert(p.clone(), Type::Str);
            }
        });
        std::borrow::Cow::Owned(crate::types::substitute(ty, &subst))
    }

    /// Returns the row that gives `ty` a must-use obligation, if any: a linear
    /// type, whose release does not discharge it. A declared row wins;
    /// otherwise the resolved type answers, seeded by `Stream`.
    pub fn linear_kind(&self, ty: &Type) -> Option<Linear> {
        if let Some(k) = crate::types::type_key(ty).filter(|k| self.linear.contains(k)) {
            return Some(Linear::Declared(k));
        }
        // A self-referring type has no bottom to walk; its obligation goes
        // unseen rather than the walk not returning.
        if self_referring(ty, &self.types).is_some() {
            return None;
        }
        match crate::types::resolve(ty, &self.types) {
            Type::Stream(_) => Some(Linear::Stream),
            // A container answers what its element does; containers
            // release their elements, so `drop` discharges each. A record field
            // or a declared enum payload does not oblige its holder: the author
            // writes `impl MustUse` on it. A type parameter answers `None`, so
            // generic containers carry no obligation.
            Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) => self.linear_kind(&e),
            Type::Map(a, b) => self.linear_kind(&a).or_else(|| self.linear_kind(&b)),
            // `Option` and `Result` answer through their payloads.
            ref r if crate::types::option_payload(r).is_some() => {
                self.linear_kind(crate::types::option_payload(r).unwrap())
            }
            ref r if crate::types::result_payloads(r).is_some() => {
                let (a, b) = crate::types::result_payloads(r).unwrap();
                self.linear_kind(a).or_else(|| self.linear_kind(b))
            }
            _ => None,
        }
    }

    /// Whether letting a value of `ty` go out of scope is an error.
    pub fn must_use(&self, ty: &Type) -> bool {
        self.linear_kind(ty).is_some()
    }

    /// The program's type declarations. Use this rather than calling
    /// [`crate::types::decl_map`], which clones them all, per node.
    pub fn types(&self) -> &HashMap<String, TypeDecl> {
        &self.types
    }

    /// Whether `ty` transitively owns heap. Differs from
    /// [`Owned::release_kind`] being `Some` where a type owns heap but has no
    /// release row, such as a `Stream` or an unbounded self-referring type.
    pub fn owns_heap(&self, ty: &Type) -> bool {
        owns_heap(&self.standing(ty), &self.types)
    }

    /// Returns the name `ty` reaches from itself with no declared release in
    /// between: the self-reference a structural release walk cannot bottom out
    /// in. Unlike [`self_referring`], the walk stops at a type with
    /// `impl Owned`, which is released by a call.
    pub fn unbounded(&self, ty: &Type) -> Option<String> {
        self_referring_past(ty, &self.types, &|n| self.impls.contains_key(n))
    }

    /// Whether `name` is a declared `release` body. A consume-match inside one
    /// must not free its payload boxes: the caller of the release walks them
    /// afterwards.
    pub fn is_release_fn(&self, name: &str) -> bool {
        self.impls.values().any(|f| f == name)
    }

    /// Returns the declared `release` bodies a release of `ty` may call, sorted:
    /// the row of every type the release walk reaches, which stops at a row. A
    /// function value, a lazy value or a stream may hold a value of any type, so
    /// each answers every row.
    pub fn declared_releases(&self, ty: &Type) -> Vec<String> {
        fn go(o: &Owned, ty: &Type, seen: &mut Vec<String>, out: &mut Vec<String>) {
            if let Some(f) = crate::types::type_key(ty).and_then(|k| o.impls.get(&k)) {
                out.push(f.clone());
                return;
            }
            if let Type::Named(n) | Type::App(n, _) = ty {
                if !seen.contains(n) && o.types.contains_key(n) {
                    seen.push(n.clone());
                    go(o, &crate::types::resolve(ty, &o.types), seen, out);
                }
                return;
            }
            match ty {
                Type::Fn(..) | Type::Lazy(_) | Type::Stream(_) => {
                    out.extend(o.impls.values().cloned())
                }
                Type::Array(t) | Type::ArrayN(t, _) | Type::SmallArray(t, _) => go(o, t, seen, out),
                Type::Map(a, b) => {
                    go(o, a, seen, out);
                    go(o, b, seen, out);
                }
                Type::Record(fs) => fs.iter().for_each(|f| go(o, &f.ty, seen, out)),
                Type::Enum(vs) => vs
                    .iter()
                    .flat_map(|v| &v.payload)
                    .for_each(|p| go(o, p, seen, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        if !self.impls.is_empty() {
            go(self, ty, &mut Vec::new(), &mut out);
        }
        out.sort();
        out.dedup();
        out
    }

    /// Returns how a value of `ty` is reclaimed, or `None` when it owns no heap
    /// or cannot be walked. A declared row wins; otherwise the resolved type
    /// answers. The match has no `_` arm, so a new [`Type`] variant must decide.
    pub fn release_kind(&self, ty: &Type) -> Option<DropKind> {
        let ty = &*self.standing(ty);
        if let Some(f) = crate::types::type_key(ty).and_then(|k| self.impls.get(&k)) {
            return Some(DropKind::Release(f.clone(), ty.clone()));
        }
        match crate::types::resolve(ty, &self.types) {
            Type::Str => Some(DropKind::FreeStr),
            // An array owns its elements (every route into one is a store), so
            // an element with a row makes it `Deep`, the release walk. A
            // self-referring element without a declared release gets the buffer
            // alone and its elements leak. A `Param` element is `Deep` too: the
            // emitters substitute the instance before walking.
            Type::Array(e) => Some(
                if self.unbounded(&e).is_none()
                    && (self.release_kind(&e).is_some()
                        || matches!(crate::types::resolve(&e, &self.types), Type::Param(_)))
                {
                    DropKind::Deep(Type::Array(e))
                } else {
                    DropKind::FreeArr
                },
            ),
            // The same recursion over a `SmallArray`'s slots and a `Map`'s keys
            // and values.
            Type::SmallArray(e, n) => {
                let t = Type::SmallArray(e.clone(), n);
                Some(
                    if self.unbounded(&t).is_none() && self.release_kind(&e).is_some() {
                        DropKind::Deep(t)
                    } else {
                        DropKind::FreeSmallArr
                    },
                )
            }
            Type::Map(k, v) => {
                let t = Type::Map(k.clone(), v.clone());
                Some(
                    if self.unbounded(&t).is_none()
                        && (self.release_kind(&k).is_some() || self.release_kind(&v).is_some())
                    {
                        DropKind::Deep(t)
                    } else {
                        DropKind::FreeMap
                    },
                )
            }
            // The stream lowering pushes its own release; answering here too
            // would release it twice.
            Type::Stream(_) => None,
            Type::Int
            | Type::IntN { .. }
            | Type::Float
            | Type::Float32
            | Type::F32x4
            | Type::I32x4
            | Type::F64x2
            | Type::Mask32x4
            | Type::Mask64x2
            | Type::Bool
            | Type::Unit
            | Type::ConstInt(_)
            | Type::Logger
            | Type::Never
            | Type::Err => None,
            // A function value's capture block is one allocation, and its
            // release walks the captures the tag names (`lower_fnval_free`).
            t @ Type::Fn(..) => Some(DropKind::Deep(t)),
            // An aggregate owns its places (a store into one is a move), so
            // releasing it releases them. A self-referring type with no
            // declared release on the cycle answers `None` and leaks.
            // `Option` and `Result` resolve to enums and take this arm.
            t @ (Type::Record(_) | Type::Enum(_) | Type::ArrayN(..)) => {
                (self.unbounded(ty).is_none() && owns_heap(&t, &self.types))
                    .then(|| DropKind::Deep(t))
            }
            // `resolve` answers `lazy T` as `fn() -> T`; this is its
            // depth-limited fallback.
            Type::Lazy(_) => None,
            // Not runtime values: an operator resolves to its base, a `Param` is
            // erased by monomorphization, and an undeclared name owns nothing
            // (`Code` is a handle; see [`owns_heap`]).
            Type::Omit(..)
            | Type::Pick(..)
            | Type::Merge(..)
            | Type::Partial(_)
            | Type::Param(_)
            | Type::Named(_)
            | Type::App(..) => None,
        }
    }
}

/// Returns the name of a type `ty` reaches from itself, if any. A structural
/// walk such as `copy` has no bottom there, so a caller refuses the type by name
/// instead of overflowing the stack; the type declares its own `Copy`.
pub fn self_referring(ty: &Type, types: &HashMap<String, TypeDecl>) -> Option<String> {
    self_referring_past(ty, types, &|_| false)
}

/// [`self_referring`], with the walk stopping at every name `stops` accepts.
fn self_referring_past(
    ty: &Type,
    types: &HashMap<String, TypeDecl>,
    stops: &dyn Fn(&str) -> bool,
) -> Option<String> {
    fn go(
        ty: &Type,
        types: &HashMap<String, TypeDecl>,
        stops: &dyn Fn(&str) -> bool,
        seen: &mut Vec<String>,
    ) -> Option<String> {
        if let Type::Named(n) | Type::App(n, _) = ty {
            if stops(n) {
                return None;
            }
            if seen.iter().any(|s| s == n) {
                return Some(n.clone());
            }
            if !types.contains_key(n) {
                return None;
            }
            seen.push(n.clone());
            let r = go(&crate::types::resolve(ty, types), types, stops, seen);
            seen.pop();
            return r;
        }
        let mut deeper = |t: &Type| go(t, types, stops, seen);
        match ty {
            Type::Array(t)
            | Type::ArrayN(t, _)
            | Type::SmallArray(t, _)
            | Type::Lazy(t)
            | Type::Stream(t) => deeper(t),
            Type::Map(a, b) => deeper(a).or_else(|| deeper(b)),
            Type::Record(fs) => fs.iter().find_map(|f| go(&f.ty, types, stops, seen)),
            Type::Enum(vs) => vs
                .iter()
                .find_map(|v| v.payload.iter().find_map(|p| go(p, types, stops, seen))),
            _ => None,
        }
    }
    go(ty, types, stops, &mut Vec::new())
}

/// Whether a value of `ty` transitively owns heap, so it
/// moves rather than copies.
pub fn owns_heap(ty: &Type, types: &HashMap<String, TypeDecl>) -> bool {
    fn go(ty: &Type, types: &HashMap<String, TypeDecl>, seen: &mut Vec<String>) -> bool {
        // A name that reaches itself owns heap: the recursive field must be
        // boxed. A depth limit here answered `false` for `type Tree` and leaked
        // every tree's boxes; do not bring one back.
        if let Type::Named(n) | Type::App(n, _) = ty {
            if seen.iter().any(|x| x == n) {
                return true;
            }
            // An undeclared name owns nothing. `Code` is such a name: a handle
            // into the generator's piece arena, with nothing guest-side to free.
            if !types.contains_key(n) {
                return false;
            }
            seen.push(n.clone());
            let r = go(&crate::types::resolve(ty, types), types, seen);
            seen.pop();
            return r;
        }
        let deeper = |t: &Type| go(t, types, &mut seen.clone());
        match crate::types::resolve(ty, types) {
            Type::Str | Type::Array(_) | Type::SmallArray(..) | Type::Map(..) | Type::Stream(_) => {
                true
            }
            Type::ArrayN(t, _) | Type::Lazy(t) => deeper(&t),
            Type::Record(fs) => fs.iter().any(|f| deeper(&f.ty)),
            // A boxed payload owns its box; `types::payload_boxed` is the
            // emitter's own rule.
            Type::Enum(vs) => vs.iter().any(|v| {
                v.payload
                    .iter()
                    .any(|t| crate::types::payload_boxed(t, types) || deeper(t))
            }),
            // A function value's capture block is heap, so a `fn` moves; if it
            // copied freely the block would be released twice. Its copy is
            // `@__vyrn_fnval_copy`, a switch on the tag's block size.
            Type::Fn(..) => true,
            _ => false,
        }
    }
    go(ty, types, &mut Vec::new())
}

/// Whether the release walk of `ty` can skip every one of `paths` (the holes
/// a `consume` left). A path is a chain of record fields and enum payloads spelled
/// `Variant.i`. A declared `release` or a container on the chain answers
/// false: a user function cannot skip a field, and the walk has no index.
/// The core states no hole this refuses (`r22_drop_with_a_hole.vyrn`).
pub fn skippable(proto: &Owned, ty: &Type, paths: &[String]) -> bool {
    paths.iter().all(|p| {
        let mut cur = ty.clone();
        let mut segs = p.split('.');
        while let Some(seg) = segs.next() {
            if matches!(proto.release_kind(&cur), Some(DropKind::Release(..))) {
                return false;
            }
            let next = match crate::types::resolve(&cur, &proto.types) {
                Type::Record(fields) => fields.into_iter().find(|f| f.name == seg).map(|f| f.ty),
                Type::Enum(vs) => segs.next().and_then(|i| {
                    let v = vs.into_iter().find(|v| v.name == seg)?;
                    v.payload.into_iter().nth(i.parse().ok()?)
                }),
                _ => None,
            };
            let Some(next) = next else {
                return false;
            };
            cur = next;
        }
        true
    })
}

/// Returns the holes inside field `name`, with its hop removed: `head.err`
/// becomes `err`, and a sibling's hole is dropped.
pub fn holes_under(holes: &[String], name: &str) -> Vec<String> {
    holes
        .iter()
        .filter_map(|h| h.strip_prefix(name)?.strip_prefix('.'))
        .map(str::to_string)
        .collect()
}

/// Whether `e` allocates a fresh String no binding names (`@str`, `@concat`,
/// `+`), so its consumer must release it. The caller must also check the type
/// is `String`, because `+` also adds integers and joins `Code`. A call result
/// is not covered here: [`crate::movecheck::ArgVerdict`] answers for it.
pub fn str_temporary(e: &Expr) -> bool {
    match e {
        Expr::Call { name, .. } => name == "@str" || name == "@concat",
        Expr::Binary { op: BinOp::Add, .. } => true,
        _ => false,
    }
}

/// The program-level tables, built once per program.
pub struct Declared {
    /// The checker's type for every node, keyed by address. `None` for a
    /// program the checker never saw.
    rec: Option<std::rc::Rc<crate::checker::Recorded>>,
    decls: HashMap<String, TypeDecl>,
    /// Declared parameter types per user function, for an argument whose own
    /// expression has no type (an array literal coerced at the call).
    params: HashMap<String, Vec<Type>>,
    owned: crate::declared::Owned,
    /// Every variant constructor to the enum it builds, or `None` where no
    /// single named type answers: a built-in sum, a name two enums share, or a
    /// generic enum.
    variants: HashMap<String, Option<String>>,
}

impl Declared {
    pub fn new(program: &Program) -> Self {
        let mut params: HashMap<String, Vec<Type>> = HashMap::new();
        for f in &program.functions {
            params.insert(
                f.name.clone(),
                f.params.iter().map(|p| p.ty.clone()).collect(),
            );
        }
        let decls = crate::types::decl_map(program);
        let mut variants: HashMap<String, Option<String>> =
            ["Some", "Ok", "Err", "Success", "Failure"]
                .into_iter()
                .map(|n| (n.to_string(), None))
                .collect();
        for d in decls.values() {
            if let Some(vs) = crate::types::declared_variants(&d.base) {
                for v in vs {
                    let owner = (d.type_params.is_empty() && !variants.contains_key(&v.name))
                        .then(|| d.name.clone());
                    variants.insert(v.name.clone(), owner);
                }
            }
        }
        Declared {
            rec: None,
            owned: crate::declared::Owned::new(program),
            variants,
            decls,
            params,
        }
    }

    /// Attaches the checker's record for this program (see
    /// [`crate::checker::recorded`]).
    pub fn recording(mut self, rec: std::rc::Rc<crate::checker::Recorded>) -> Self {
        self.rec = Some(rec);
        self
    }

    pub fn decls(&self) -> &HashMap<String, TypeDecl> {
        &self.decls
    }

    pub fn owns_heap(&self, ty: &Type) -> bool {
        crate::declared::owns_heap(ty, &self.decls)
    }

    pub fn linear_kind(&self, ty: &Type) -> Option<crate::own::Linear> {
        self.owned.linear_kind(ty)
    }

    /// Whether whoever holds a value of `ty` releases it. Unlike
    /// [`Declared::owns_heap`], a type with no release row (a `Stream`) answers
    /// false.
    pub fn releases(&self, ty: &Type) -> bool {
        self.release_kind(ty).is_some()
    }

    pub fn release_kind(&self, ty: &Type) -> Option<crate::own::DropKind> {
        self.owned.release_kind(ty)
    }

    /// Whether `name` is a variant constructor, whose value holds its argument
    /// past the call.
    pub fn constructs(&self, name: &str) -> bool {
        self.variants.contains_key(name)
    }

    /// The declared type of `callee`'s parameter `ix`.
    pub fn param_ty(&self, callee: &str, ix: usize) -> Option<&Type> {
        self.params.get(callee).and_then(|ps| ps.get(ix))
    }
}

/// Returns each callee's parameter capabilities: every function's, then every
/// protocol method's (receiver first) over them, since a method call arrives
/// under its surface name. `movecheck::arg_verdict` and the core both read it.
pub fn arg_caps(program: &Program) -> HashMap<String, Vec<Capability>> {
    let mut caps: HashMap<String, Vec<Capability>> = program
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.capability).collect(),
            )
        })
        .collect();
    for p in &program.protocols {
        for m in &p.methods {
            let mut cs = vec![m.recv];
            cs.extend(m.param_caps.iter().copied());
            caps.insert(m.name.clone(), cs);
        }
    }
    caps
}

/// Returns the capability of one position: the declaration's, else the seeded
/// row's. `None` is [`crate::movecheck::ArgVerdict::Unknown`], which frees
/// nothing.
pub fn arg_cap(
    caps: &HashMap<String, Vec<Capability>>,
    callee: &str,
    ix: usize,
) -> Option<Capability> {
    caps.get(callee)
        .and_then(|c| c.get(ix))
        .copied()
        .or_else(|| crate::prelude::capability(callee, ix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};

    /// The obligation comes out of the program: nothing in the compiler knows
    /// `Txn`.
    #[test]
    fn a_user_type_declares_that_it_must_be_used() {
        let src = "protocol MustUse {} \
                   type Txn = { id: Int64 } \
                   impl MustUse for Txn {} \
                   type Plain = { id: Int64 } \
                   fn main() -> Int64 { return 0 }";
        let p = parse(lex(src).unwrap()).unwrap();
        let owned = Owned::new(&p);
        let txn = Type::Named("Txn".into());
        assert_eq!(
            owned.linear_kind(&txn),
            Some(Linear::Declared("Txn".into()))
        );
        assert_eq!(owned.linear_kind(&Type::Named("Plain".into())), None);
        // A container answers with the row of the type that declared it.
        assert_eq!(
            owned.linear_kind(&Type::Array(Box::new(txn.clone()))),
            Some(Linear::Declared("Txn".into()))
        );
        assert_eq!(
            owned.linear_kind(&Type::option(Type::Map(Box::new(Type::Str), Box::new(txn)))),
            Some(Linear::Declared("Txn".into()))
        );
        assert_eq!(
            owned.linear_kind(&Type::Array(Box::new(Type::Named("Plain".into())))),
            None
        );
        assert_eq!(
            owned.linear_kind(&Type::Array(Box::new(Type::Param("T".into())))),
            None
        );
        // The seeded row needs no declarations.
        assert_eq!(
            Owned::default().linear_kind(&Type::Stream(Box::new(Type::Int))),
            Some(Linear::Stream)
        );
    }

    /// A depth bound in `owns_heap` would bring back a silent leak of every
    /// recursive enum's boxes.
    #[test]
    fn a_self_referring_type_owns_heap() {
        use crate::ast::{EnumVariant, TypeDecl};
        let mut types: HashMap<String, TypeDecl> = HashMap::new();
        types.insert(
            "Tree".to_string(),
            TypeDecl {
                name: "Tree".to_string(),
                base: Type::Enum(vec![
                    EnumVariant {
                        name: "Leaf".to_string(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "Node".to_string(),
                        payload: vec![
                            Type::Named("Tree".to_string()),
                            Type::Named("Tree".to_string()),
                        ],
                    },
                ]),
                exported: false,
                module: None,
                doc: None,
                type_params: vec![],
                predicate: None,
                line: 1,
            },
        );
        assert!(
            owns_heap(&Type::Named("Tree".to_string()), &types),
            "a recursive enum owns the boxes its payloads travel in"
        );
        assert!(
            !owns_heap(&Type::Int, &types),
            "an integer owns nothing, and the cycle rule must not change that"
        );
    }
}
