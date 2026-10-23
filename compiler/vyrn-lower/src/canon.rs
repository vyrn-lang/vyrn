//! Relation identity: the canonical form of a loop-free, state-free body, so
//! that two relations a fact names are one relation when their forms agree.
//! The census `vyrn-cli/tests/canon.rs` reads it; nothing in the compiler does.
//!
//! [`Table::relation`] turns a predicate, a key lambda or a loop-free function
//! into a tree over its parameters by position: each name becomes the tree of
//! the value it holds, and an `if`, the core's form of `&&` and `||` included,
//! an [`Node::Ite`]. [`Table::conditions`] gives each `if` and `while`
//! condition of a body its tree, with the names the reader bound as leaves.
//! [`Table::identity`] then decides at a [`Level`]; each level is a rewrite
//! that keeps the value and the traps of the body on every input.
//!
//! Every tree, atom and decision diagram is hash-consed in one [`Table`] per
//! program, so an identity is an [`Id`]. Two identities of one table are one
//! exactly when their structures are; [`Table::key`] spells an identity for a
//! comparison across tables. Atoms are ordered by their keys, never by the
//! order a body met them.

use std::collections::HashMap;
use std::rc::Rc;

use vyrn_frontend::ast::{BinOp, Capability, NodeId, Type, UnOp};
use vyrn_frontend::core::{
    Arg, Body, BorrowKind, Callee, Lit, Name, Op, Place, Rhs, St, Target, Val,
};
use vyrn_frontend::types::{resolved, Decls};

/// The largest Boolean structure decided whole: its distinct atoms, and the
/// nodes of its two decision diagrams together. Past either, the form
/// degrades step by step ([`Table::identity`]). `atoms * nodes` bounds the
/// steps of one diagram build, and `nodes` the rows one walk of a body reads.
pub struct Budget {
    pub atoms: usize,
    pub nodes: usize,
}

pub const BUDGET: Budget = Budget {
    atoms: 16,
    nodes: 1024,
};

/// A node of a [`Table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(u32);

/// A node of a table's decision diagrams; [`FALSE`] and [`TRUE`] are the leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Ref(u32);

const FALSE: Ref = Ref(0);
const TRUE: Ref = Ref(1);

/// An operand type, resolved through the program's declarations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// Arithmetic wraps at `bits`: 8 to 64 in a program, any width from 1 the
    /// brute force asks for.
    Int {
        bits: u8,
        signed: bool,
    },
    Float(u8),
    Bool,
    Str,
    /// Any other type, by its debug spelling.
    Other(String),
}

impl Ty {
    fn of(t: &Type, decls: &dyn Decls) -> Ty {
        match &*resolved(t, decls) {
            Type::Int => Ty::Int {
                bits: 64,
                signed: true,
            },
            Type::IntN { bits, signed } => Ty::Int {
                bits: *bits,
                signed: *signed,
            },
            Type::Float => Ty::Float(64),
            Type::Float32 => Ty::Float(32),
            Type::Bool => Ty::Bool,
            Type::Str => Ty::Str,
            t => Ty::Other(format!("{t:?}")),
        }
    }

    fn spell(&self) -> String {
        match self {
            Ty::Int { bits, signed } => format!("{}{bits}", if *signed { "i" } else { "u" }),
            Ty::Float(bits) => format!("f{bits}"),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::Other(s) => s.clone(),
        }
    }
}

/// An operator of a [`Node::Prim`]: the core's [`BinOp`] and [`UnOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Pr {
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
    Match,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Neg,
    Not,
    BitNot,
}

impl Pr {
    fn bin(op: BinOp) -> Pr {
        match op {
            BinOp::Add => Pr::Add,
            BinOp::Sub => Pr::Sub,
            BinOp::Mul => Pr::Mul,
            BinOp::Div => Pr::Div,
            BinOp::Rem => Pr::Rem,
            BinOp::Eq => Pr::Eq,
            BinOp::NotEq => Pr::Ne,
            BinOp::Lt => Pr::Lt,
            BinOp::LtEq => Pr::Le,
            BinOp::Gt => Pr::Gt,
            BinOp::GtEq => Pr::Ge,
            BinOp::And => Pr::And,
            BinOp::Or => Pr::Or,
            BinOp::Match => Pr::Match,
            BinOp::BitAnd => Pr::BitAnd,
            BinOp::BitOr => Pr::BitOr,
            BinOp::BitXor => Pr::BitXor,
            BinOp::Shl => Pr::Shl,
            BinOp::Shr => Pr::Shr,
        }
    }

    fn un(op: UnOp) -> Pr {
        match op {
            UnOp::Neg => Pr::Neg,
            UnOp::Not => Pr::Not,
            UnOp::BitNot => Pr::BitNot,
        }
    }
}

/// A node: a tree of a body's values, an atom normalized by layer 2, or a
/// form of layer 1. Children are ids of the same table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Node {
    /// A parameter of a relation, by position among those that are not
    /// captures.
    Param(u32, Ty),
    /// A capture, by the enclosing frame's spelling.
    Free(String, Ty),
    /// A name the reader bound, in a condition's tree.
    Local(Name, Ty),
    Global(String),
    Int(i64),
    /// The bits of a float literal.
    Float(u64),
    Bool(bool),
    Str(String),
    Opaque,
    Field(Id, String),
    Index(Id, Id),
    Key(Id, Id),
    /// A call, by the callee's spelling: the function value first for a call
    /// through one, then the arguments, then each bound target; and whether
    /// the call may trap.
    Call(String, Vec<Id>, bool),
    /// An operator and its operands' type. `==` and `!=` hold their operands
    /// in key order.
    Prim(Pr, Ty, Vec<Id>),
    Conv(Ty, Id),
    Closure(String, Vec<Id>),
    Make(String, Vec<Id>),
    /// A function or lambda a call binds to a `fn` parameter, by its spelling.
    Static(String),
    Ite(Id, Id, Id),
    /// A path that traps.
    Trap,
    /// `c + sum(k * t)` modulo `2^bits`, the terms in key order, each
    /// coefficient a nonzero residue.
    Lin(Ty, Vec<(Id, u64)>, u64),
    /// A linear form, of the two signs the one whose key is less, is zero.
    Eq0(Ty, Id),
    /// That the element evaluates without a trap: a definedness variable.
    Def(Id),
    /// The answers-true and answers-false diagrams of a Boolean structure.
    Form(Ref, Ref),
    /// The conjunction of trap-free parts over disjoint atoms, in key order.
    All(Vec<Id>),
}

/// Which rewrites decide identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Layer 0: the tree, after resolution, with `==` and `!=` operands
    /// ordered.
    Syntax,
    /// Layer 1: the Boolean structure as a decision diagram over atoms of
    /// layer 0, `a != b` as `!(a == b)`.
    Boolean,
    /// Layer 2: atoms normalized by operand type.
    Atoms,
}

/// Why a body has no relation tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unfit {
    /// A `while` or `for`.
    Loop,
    /// A `match`, an `if let` or a `?`.
    Switch,
    /// A store into a place other than a local, or a `modify` argument.
    Writes,
    /// A call for its effect alone.
    Effect,
    /// A path that ends without a value.
    NoValue,
    /// A name read before any row binds it.
    Unbound,
    /// More rows than [`BUDGET`]'s nodes on one walk.
    Budget,
}

/// The value tree of a relation and the type it answers.
#[derive(Debug, Clone)]
pub struct Relation {
    pub root: Id,
    pub ty: Ty,
}

/// How much of a Boolean structure was decided whole, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// One pair of diagrams in the order of the atoms' keys, within budget.
    Whole,
    /// Conjuncts over disjoint atoms, each decided on its own.
    Split,
    /// One pair of diagrams in an order sifting found.
    Sifted,
    /// Each subterm that fits decided, the rest kept as syntax.
    Partial,
}

/// An identity, with what deciding it took: the distinct atoms of its
/// Boolean structure, the nodes of its diagrams where it is one form, and
/// the last step of [`Step`] it needed. Not a Boolean structure: no atoms
/// and [`Step::Whole`].
#[derive(Debug, Clone, Copy)]
pub struct Canon {
    pub id: Id,
    pub atoms: usize,
    pub nodes: usize,
    pub step: Step,
}

/// The shape of a node at a Boolean position.
enum Shape {
    Const(bool),
    Not(Id),
    Ite(Id, Id, Id),
    /// `==` (true) or `!=` between Bools.
    Same(Id, Id, bool),
    Trap,
    Atom,
}

/// An atom's value variable, or its constant, and whether it is negated.
#[derive(Clone, Copy)]
enum Value {
    Const(bool),
    Var(Id, bool),
}

/// What the diagram builders read of an atom: its value and the
/// definedness variables of its trapping elements.
struct Atom {
    value: Value,
    defs: Vec<Id>,
}

/// Hash-consed decision-diagram nodes: a variable, the low and the high
/// child. Indexes 0 and 1 are the leaves.
#[derive(Default)]
struct Bdd {
    nodes: Vec<(Id, Ref, Ref)>,
    ids: HashMap<(Id, Ref, Ref), Ref>,
}

#[derive(Clone, Copy)]
enum Op2 {
    And,
    Or,
}

impl Bdd {
    fn new() -> Bdd {
        let leaf = (Id(u32::MAX), FALSE, FALSE);
        Bdd {
            nodes: vec![leaf, leaf],
            ids: HashMap::new(),
        }
    }

    fn mk(&mut self, v: Id, lo: Ref, hi: Ref) -> Ref {
        if lo == hi {
            return lo;
        }
        let next = Ref(self.nodes.len() as u32);
        *self.ids.entry((v, lo, hi)).or_insert_with(|| {
            self.nodes.push((v, lo, hi));
            next
        })
    }

    fn leaf(r: Ref) -> bool {
        r.0 < 2
    }

    /// `op(a, b)` under the variable order `rank`; `None` once the build has
    /// taken `work` steps.
    fn apply(
        &mut self,
        op: Op2,
        a: Ref,
        b: Ref,
        rank: &HashMap<Id, usize>,
        work: &mut usize,
    ) -> Option<Ref> {
        let mut memo = HashMap::new();
        self.apply_in(op, a, b, rank, work, &mut memo)
    }

    fn apply_in(
        &mut self,
        op: Op2,
        a: Ref,
        b: Ref,
        rank: &HashMap<Id, usize>,
        work: &mut usize,
        memo: &mut HashMap<(Ref, Ref), Ref>,
    ) -> Option<Ref> {
        match op {
            Op2::And if a == FALSE || b == FALSE => return Some(FALSE),
            Op2::And if a == TRUE => return Some(b),
            Op2::And if b == TRUE || a == b => return Some(a),
            Op2::Or if a == TRUE || b == TRUE => return Some(TRUE),
            Op2::Or if a == FALSE => return Some(b),
            Op2::Or if b == FALSE || a == b => return Some(a),
            _ => {}
        }
        if let Some(r) = memo.get(&(a, b)) {
            return Some(*r);
        }
        *work = work.checked_sub(1)?;
        let rank_of = |r: Ref, nodes: &[(Id, Ref, Ref)]| {
            (!Bdd::leaf(r)).then(|| rank[&nodes[r.0 as usize].0])
        };
        let (ra, rb) = (rank_of(a, &self.nodes), rank_of(b, &self.nodes));
        let top = match (ra, rb) {
            (Some(x), Some(y)) => x.min(y),
            (Some(x), None) => x,
            (None, Some(y)) => y,
            (None, None) => unreachable!("two leaves are decided above"),
        };
        let split = |r: Ref, rr: Option<usize>, nodes: &[(Id, Ref, Ref)]| match rr {
            Some(x) if x == top => (nodes[r.0 as usize].1, nodes[r.0 as usize].2),
            _ => (r, r),
        };
        let (alo, ahi) = split(a, ra, &self.nodes);
        let (blo, bhi) = split(b, rb, &self.nodes);
        let v = if ra == Some(top) {
            self.nodes[a.0 as usize].0
        } else {
            self.nodes[b.0 as usize].0
        };
        let lo = self.apply_in(op, alo, blo, rank, work, memo)?;
        let hi = self.apply_in(op, ahi, bhi, rank, work, memo)?;
        let r = self.mk(v, lo, hi);
        memo.insert((a, b), r);
        Some(r)
    }

    /// The nodes reachable from `roots`, leaves excluded.
    fn size(&self, roots: &[Ref]) -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut todo: Vec<Ref> = roots.to_vec();
        while let Some(r) = todo.pop() {
            if Bdd::leaf(r) || !seen.insert(r) {
                continue;
            }
            let (_, lo, hi) = self.nodes[r.0 as usize];
            todo.extend([lo, hi]);
        }
        seen.len()
    }
}

/// One program's hash-consed trees, atoms and diagrams.
pub struct Table<'d> {
    decls: &'d dyn Decls,
    /// Whether a call row of a body may trap ([`crate::verdict::TrapFree`]).
    calls: &'d dyn Fn(&Body, &Rhs) -> bool,
    nodes: Vec<Node>,
    ids: HashMap<Node, Id>,
    keys: Vec<Option<Rc<str>>>,
    bdd: Bdd,
}

impl<'d> Table<'d> {
    pub fn new(decls: &'d dyn Decls, calls: &'d dyn Fn(&Body, &Rhs) -> bool) -> Table<'d> {
        Table {
            decls,
            calls,
            nodes: Vec::new(),
            ids: HashMap::new(),
            keys: Vec::new(),
            bdd: Bdd::new(),
        }
    }

    fn intern(&mut self, n: Node) -> Id {
        if let Some(id) = self.ids.get(&n) {
            return *id;
        }
        let id = Id(self.nodes.len() as u32);
        self.nodes.push(n.clone());
        self.keys.push(None);
        self.ids.insert(n, id);
        id
    }

    fn node(&self, id: Id) -> &Node {
        &self.nodes[id.0 as usize]
    }

    /// `op` over `args`, the operands of `==` and `!=` in key order.
    fn prim(&mut self, op: Pr, ty: Ty, mut args: Vec<Id>) -> Id {
        if matches!(op, Pr::Eq | Pr::Ne) && args.len() == 2 && self.key(args[1]) < self.key(args[0])
        {
            args.swap(0, 1);
        }
        self.intern(Node::Prim(op, ty, args))
    }

    /// The canonical spelling of `id`: equal exactly when the structures are,
    /// in this table or another. A node met twice is spelled once and then
    /// referred to by its number.
    pub fn key(&mut self, id: Id) -> Rc<str> {
        if let Some(k) = &self.keys[id.0 as usize] {
            return k.clone();
        }
        let mut out = String::new();
        self.spell(id, &mut out, &mut HashMap::new(), &mut HashMap::new());
        let k: Rc<str> = out.into();
        self.keys[id.0 as usize] = Some(k.clone());
        k
    }

    fn spell(
        &self,
        id: Id,
        out: &mut String,
        seen: &mut HashMap<Id, usize>,
        bseen: &mut HashMap<Ref, usize>,
    ) {
        use std::fmt::Write;
        let node = self.node(id);
        let leaf = match node {
            Node::Param(i, t) => Some(format!("p{i}:{}", t.spell())),
            Node::Free(s, t) => Some(format!("f{s:?}:{}", t.spell())),
            Node::Local(n, t) => Some(format!("l{}:{}", n.0, t.spell())),
            Node::Global(s) => Some(format!("g{s:?}")),
            Node::Int(v) => Some(v.to_string()),
            Node::Float(b) => Some(format!("f#{b:x}")),
            Node::Bool(b) => Some(b.to_string()),
            Node::Str(s) => Some(format!("{s:?}")),
            Node::Opaque => Some("opaque".into()),
            Node::Static(s) => Some(format!("s{s:?}")),
            Node::Trap => Some("trap".into()),
            _ => None,
        };
        if let Some(l) = leaf {
            return out.push_str(&l);
        }
        let kids = self.kids(id);
        if let Some(k) = seen.get(&id) {
            let _ = write!(out, "#{k}");
            return;
        }
        let k = seen.len();
        seen.insert(id, k);
        let _ = write!(out, "#{k}=(");
        let _ = match node {
            Node::Field(_, f) => write!(out, "field {f:?}"),
            Node::Conv(t, _) => write!(out, "conv {}", t.spell()),
            Node::Def(_) => write!(out, "def"),
            Node::Index(..) => write!(out, "index"),
            Node::Key(..) => write!(out, "key"),
            Node::Call(c, _, traps) => write!(out, "call {c:?} {traps}"),
            Node::Prim(op, t, _) => write!(out, "{op:?} {}", t.spell()),
            Node::Closure(k, _) => write!(out, "closure {k:?}"),
            Node::Make(k, _) => write!(out, "make {k:?}"),
            Node::All(_) => write!(out, "all"),
            Node::Ite(..) => write!(out, "ite"),
            Node::Lin(t, ts, c) => {
                let ks: Vec<String> = ts.iter().map(|t| t.1.to_string()).collect();
                write!(out, "lin {} {c} [{}]", t.spell(), ks.join(" "))
            }
            Node::Eq0(t, _) => write!(out, "eq0 {}", t.spell()),
            Node::Form(t, f) => {
                out.push_str("form ");
                self.spell_ref(*t, out, seen, bseen);
                out.push(' ');
                self.spell_ref(*f, out, seen, bseen);
                Ok(())
            }
            _ => unreachable!("leaves are spelled above"),
        };
        for c in kids {
            out.push(' ');
            self.spell(c, out, seen, bseen);
        }
        out.push(')');
    }

    fn spell_ref(
        &self,
        r: Ref,
        out: &mut String,
        seen: &mut HashMap<Id, usize>,
        bseen: &mut HashMap<Ref, usize>,
    ) {
        use std::fmt::Write;
        if Bdd::leaf(r) {
            return out.push(if r == TRUE { 'T' } else { 'F' });
        }
        if let Some(k) = bseen.get(&r) {
            let _ = write!(out, "@{k}");
            return;
        }
        let k = bseen.len();
        bseen.insert(r, k);
        let (v, lo, hi) = self.bdd.nodes[r.0 as usize];
        let _ = write!(out, "@{k}=[");
        self.spell(v, out, seen, bseen);
        out.push(' ');
        self.spell_ref(hi, out, seen, bseen);
        out.push(' ');
        self.spell_ref(lo, out, seen, bseen);
        out.push(']');
    }

    /// The value tree of `body`, a lambda frame or a function, when its rows
    /// are loop-free and write no place but a local. Parameters are numbered
    /// by position; a capture is named by its spelling.
    pub fn relation(&mut self, body: &Body) -> Result<Relation, Unfit> {
        let mut env: Env = vec![None; body.names.len()];
        let mut at = 0;
        for &p in &body.params {
            let info = &body.names[p.index()];
            let ty = Ty::of(&info.ty, self.decls);
            env[p.index()] = Some(match info.borrow_kind {
                Some(BorrowKind::Capture) => self.intern(Node::Free(info.source.clone(), ty)),
                _ => {
                    at += 1;
                    self.intern(Node::Param(at - 1, ty))
                }
            });
        }
        let mut walk = Walk {
            t: self,
            body,
            relation: true,
            rows: 0,
            conds: Vec::new(),
        };
        let Out::Ret(root) = walk.run(body.stmts.iter().collect(), env)? else {
            return Err(Unfit::NoValue);
        };
        let ty = returned(body).map_or(Ty::Other(String::new()), |t| Ty::of(t, self.decls));
        Ok(Relation { root, ty })
    }

    /// The tree of each `if` condition of `body` by the site of its `if`, and
    /// of each `while` condition by the site of its loop, in row order. The
    /// names the reader bound and the parameters are leaves; the core's
    /// temporaries are replaced by their values.
    pub fn conditions(&mut self, body: &Body) -> Vec<(NodeId, Id)> {
        let mut walk = Walk {
            t: self,
            body,
            relation: false,
            rows: 0,
            conds: Vec::new(),
        };
        let mut env = vec![None; body.names.len()];
        walk.walk(&body.stmts, &mut env);
        walk.conds
    }

    /// The identity of a tree answering `ty` at `level`. A Bool tree at layer
    /// 1 or 2 is its decision diagrams; an integer tree at layer 2 its linear
    /// form; anything else the tree.
    pub fn identity(&mut self, root: Id, ty: &Ty, level: Level) -> Canon {
        let whole = |id| Canon {
            id,
            atoms: 0,
            nodes: 0,
            step: Step::Whole,
        };
        match (level, ty) {
            (Level::Syntax, _) => whole(root),
            (_, Ty::Bool) => self.canon(root, level),
            (Level::Atoms, Ty::Int { bits, .. }) => {
                let (ts, c) = self.lin(*bits, ty, root);
                whole(self.intern(Node::Lin(ty.clone(), ts, c)))
            }
            _ => whole(root),
        }
    }

    /// The negation of a Bool identity: the two diagrams swapped where it is
    /// one form, else a `!` around it.
    pub fn not(&mut self, id: Id) -> Id {
        match *self.node(id) {
            Node::Form(t, f) => self.intern(Node::Form(f, t)),
            _ => self.prim(Pr::Not, Ty::Bool, vec![id]),
        }
    }

    fn shape(&self, id: Id) -> Shape {
        match self.node(id) {
            Node::Bool(b) => Shape::Const(*b),
            Node::Prim(Pr::Not, _, xs) => Shape::Not(xs[0]),
            Node::Prim(op @ (Pr::Eq | Pr::Ne), Ty::Bool, xs) => {
                Shape::Same(xs[0], xs[1], *op == Pr::Eq)
            }
            Node::Ite(c, a, b) => Shape::Ite(*c, *a, *b),
            Node::Trap => Shape::Trap,
            _ => Shape::Atom,
        }
    }

    /// The Boolean identity of `root`, degrading past [`BUDGET`]: whole in
    /// the keys' order; else split into conjuncts over disjoint atoms; else
    /// in an order sifting finds; else each subterm on its own.
    fn canon(&mut self, root: Id, level: Level) -> Canon {
        let mut atoms = HashMap::new();
        self.atoms(
            root,
            level,
            &mut atoms,
            &mut std::collections::HashSet::new(),
        );
        let (vals, mut vars) = vars_of(&atoms);
        let n = vals.len();
        let mut keyed: Vec<(Rc<str>, Id)> = vars.drain(..).map(|v| (self.key(v), v)).collect();
        keyed.sort();
        let order: Vec<Id> = keyed.into_iter().map(|k| k.1).collect();
        let done = |id, nodes, step| Canon {
            id,
            atoms: n,
            nodes,
            step,
        };
        if n <= BUDGET.atoms {
            if let Some((t, f, size)) = self.fits(root, &atoms, &order) {
                return done(self.intern(Node::Form(t, f)), size, Step::Whole);
            }
        }
        if let Some((id, step)) = self.split(root, level) {
            return done(id, 0, step);
        }
        if n <= BUDGET.atoms {
            if let Some(order) = self.sift(root, &atoms, order) {
                let (t, f, size) = self
                    .fits(root, &atoms, &order)
                    .expect("the sifted order fits");
                return done(self.intern(Node::Form(t, f)), size, Step::Sifted);
            }
        }
        let id = self.partial(root, level);
        done(id, 0, Step::Partial)
    }

    /// The pair of diagrams of `root` in the table under `order`, and its
    /// size, when it is within budget.
    fn fits(
        &mut self,
        root: Id,
        atoms: &HashMap<Id, Atom>,
        order: &[Id],
    ) -> Option<(Ref, Ref, usize)> {
        let rank = order.iter().enumerate().map(|(i, v)| (*v, i)).collect();
        let mut work = BUDGET.atoms * BUDGET.nodes;
        let mut bdd = std::mem::take(&mut self.bdd);
        let r = self.build(&mut bdd, root, atoms, &rank, &mut work, &mut HashMap::new());
        let size = r.map(|(t, f)| bdd.size(&[t, f]));
        self.bdd = bdd;
        let (t, f) = r?;
        let size = size?;
        (size <= BUDGET.nodes).then_some((t, f, size))
    }

    /// The order sifting reaches from `order`: each variable in turn, in
    /// `order`'s order, moves to the position where the pair is smallest,
    /// the first such. Trials build in a scratch table. `None` when no
    /// order it tries fits the budget.
    fn sift(&mut self, root: Id, atoms: &HashMap<Id, Atom>, mut order: Vec<Id>) -> Option<Vec<Id>> {
        let size_of = |t: &Table<'_>, order: &[Id]| {
            let rank = order.iter().enumerate().map(|(i, v)| (*v, i)).collect();
            let mut work = BUDGET.atoms * BUDGET.nodes;
            let mut scratch = Bdd::new();
            let (a, b) = t.build(
                &mut scratch,
                root,
                atoms,
                &rank,
                &mut work,
                &mut HashMap::new(),
            )?;
            Some(scratch.size(&[a, b]))
        };
        let mut best = size_of(self, &order);
        for v in order.clone() {
            let from = order
                .iter()
                .position(|x| *x == v)
                .expect("v is in the order");
            order.remove(from);
            let mut at = from;
            for i in 0..=order.len() {
                order.insert(i, v);
                let s = size_of(self, &order);
                if s.is_some() && (best.is_none() || s < best) {
                    (best, at) = (s, i);
                }
                order.remove(i);
            }
            order.insert(at, v);
        }
        best.filter(|s| *s <= BUDGET.nodes).map(|_| order)
    }

    /// Splits `root` into its conjuncts and groups them by shared atoms. Each
    /// group is decided on its own, so the identity is the set of the
    /// groups'. Only for trap-free conjuncts, because moving a conjunct past
    /// one that traps changes which trap a body meets; and only into two
    /// groups or more.
    fn split(&mut self, root: Id, level: Level) -> Option<(Id, Step)> {
        let mut parts = Vec::new();
        self.conjuncts(root, &mut parts);
        if parts.len() < 2 {
            return None;
        }
        let mut sets = Vec::new();
        for &p in &parts {
            let mut mine = HashMap::new();
            self.atoms(p, level, &mut mine, &mut std::collections::HashSet::new());
            if mine.values().any(|a| !a.defs.is_empty()) || self.has_trap(p) {
                return None;
            }
            let (vals, _) = vars_of(&mine);
            sets.push(vals);
        }
        // Union by shared atoms: `group[i]` is the least conjunct joined to `i`.
        let mut group: Vec<usize> = (0..parts.len()).collect();
        let find = |g: &Vec<usize>, mut i: usize| {
            while g[i] != i {
                i = g[i];
            }
            i
        };
        for i in 0..parts.len() {
            for j in 0..i {
                if sets[i].iter().any(|v| sets[j].contains(v)) {
                    let (a, b) = (find(&group, i), find(&group, j));
                    group[a.max(b)] = a.min(b);
                }
            }
        }
        let roots: Vec<usize> = (0..parts.len()).map(|i| find(&group, i)).collect();
        let mut heads = roots.clone();
        heads.sort_unstable();
        heads.dedup();
        if heads.len() < 2 {
            return None;
        }
        let mut step = Step::Split;
        let mut forms = Vec::new();
        for h in heads {
            let mine: Vec<Id> = (0..parts.len())
                .filter(|i| roots[*i] == h)
                .map(|i| parts[i])
                .collect();
            let f = self.false_();
            let tree = (mine.iter().rev().copied())
                .reduce(|rest, c| self.intern(Node::Ite(c, rest, f)))
                .expect("a group holds a conjunct");
            let c = self.canon(tree, level);
            step = step.max(c.step);
            forms.push(c.id);
        }
        let mut keyed: Vec<(Rc<str>, Id)> = forms.into_iter().map(|f| (self.key(f), f)).collect();
        keyed.sort();
        let all = self.intern(Node::All(keyed.into_iter().map(|k| k.1).collect()));
        Some((all, step))
    }

    fn false_(&mut self) -> Id {
        self.intern(Node::Bool(false))
    }

    /// The conjuncts of `id`: `a && b` is the core's `if a { b } else { false }`.
    fn conjuncts(&self, id: Id, out: &mut Vec<Id>) {
        match self.shape(id) {
            Shape::Ite(c, a, b) if matches!(self.node(b), Node::Bool(false)) => {
                self.conjuncts(c, out);
                self.conjuncts(a, out);
            }
            _ => out.push(id),
        }
    }

    fn has_trap(&self, id: Id) -> bool {
        match self.shape(id) {
            Shape::Trap => true,
            Shape::Not(x) => self.has_trap(x),
            Shape::Ite(c, a, b) => self.has_trap(c) || self.has_trap(a) || self.has_trap(b),
            Shape::Same(a, b, _) => self.has_trap(a) || self.has_trap(b),
            Shape::Const(_) | Shape::Atom => false,
        }
    }

    /// The structure of `id` kept, each Boolean part that fits decided.
    fn partial(&mut self, id: Id, level: Level) -> Id {
        let part = |t: &mut Table<'_>, x| t.canon(x, level).id;
        match self.shape(id) {
            Shape::Not(x) => {
                let x = part(self, x);
                self.not(x)
            }
            Shape::Ite(c, a, b) => {
                let (c, a, b) = (part(self, c), part(self, a), part(self, b));
                self.intern(Node::Ite(c, a, b))
            }
            Shape::Same(a, b, eq) => {
                let (a, b) = (part(self, a), part(self, b));
                let op = if eq { Pr::Eq } else { Pr::Ne };
                self.prim(op, Ty::Bool, vec![a, b])
            }
            Shape::Const(_) | Shape::Trap | Shape::Atom => id,
        }
    }

    /// Each atom of the Boolean structure of `id`, normalized at `level`.
    fn atoms(
        &mut self,
        id: Id,
        level: Level,
        out: &mut HashMap<Id, Atom>,
        seen: &mut std::collections::HashSet<Id>,
    ) {
        if !seen.insert(id) {
            return;
        }
        match self.shape(id) {
            Shape::Const(_) | Shape::Trap => {}
            Shape::Not(x) => self.atoms(x, level, out, seen),
            Shape::Ite(c, a, b) => {
                for x in [c, a, b] {
                    self.atoms(x, level, out, seen);
                }
            }
            Shape::Same(a, b, _) => {
                self.atoms(a, level, out, seen);
                self.atoms(b, level, out, seen);
            }
            Shape::Atom => {
                let value = self.atom(id, level);
                let mut els = Vec::new();
                self.elements(id, &mut els);
                let mut defs: Vec<(Rc<str>, Id)> = (els.into_iter())
                    .map(|e| {
                        let d = self.intern(Node::Def(e));
                        (self.key(d), d)
                    })
                    .collect();
                defs.sort();
                defs.dedup();
                let defs = defs.into_iter().map(|d| d.1).collect();
                out.insert(id, Atom { value, defs });
            }
        }
    }

    /// The maximal subterms of `id` whose own evaluation may trap: an index
    /// or a key, a call whose callee is not trap-free, a division or
    /// remainder, a shift by a non-literal, and a conditional over any of
    /// these, whose parts may not run. `id` traps exactly when one of them
    /// does.
    fn elements(&self, id: Id, out: &mut Vec<Id>) {
        let own = match self.node(id) {
            Node::Index(..) | Node::Key(..) | Node::Call(_, _, true) | Node::Trap => true,
            Node::Prim(Pr::Div | Pr::Rem, ..) => true,
            Node::Prim(Pr::Shl | Pr::Shr, _, xs) => !matches!(self.node(xs[1]), Node::Int(_)),
            Node::Ite(..) => self.may_trap(id),
            _ => false,
        };
        if own {
            return out.push(id);
        }
        for c in self.kids(id) {
            self.elements(c, out);
        }
    }

    fn may_trap(&self, id: Id) -> bool {
        let mut els = Vec::new();
        for c in self.kids(id) {
            self.elements(c, &mut els);
        }
        !els.is_empty()
    }

    fn kids(&self, id: Id) -> Vec<Id> {
        match self.node(id) {
            Node::Field(b, _) | Node::Conv(_, b) | Node::Def(b) | Node::Eq0(_, b) => vec![*b],
            Node::Index(a, b) | Node::Key(a, b) => vec![*a, *b],
            Node::Call(_, xs, _)
            | Node::Prim(_, _, xs)
            | Node::Closure(_, xs)
            | Node::Make(_, xs)
            | Node::All(xs) => xs.clone(),
            Node::Ite(c, a, b) => vec![*c, *a, *b],
            Node::Lin(_, ts, _) => ts.iter().map(|t| t.0).collect(),
            _ => Vec::new(),
        }
    }

    /// The pair of diagrams of `id`, answers true and answers false, in
    /// `bdd` under `rank`. McCarthy's sequential logic: an atom answers only
    /// where its elements are defined, `&&`, `||` and `if` evaluate left to
    /// right, and a trap answers neither.
    fn build(
        &self,
        bdd: &mut Bdd,
        id: Id,
        atoms: &HashMap<Id, Atom>,
        rank: &HashMap<Id, usize>,
        work: &mut usize,
        memo: &mut HashMap<Id, (Ref, Ref)>,
    ) -> Option<(Ref, Ref)> {
        if let Some(r) = memo.get(&id) {
            return Some(*r);
        }
        let and = |bdd: &mut Bdd, a, b, w: &mut usize| bdd.apply(Op2::And, a, b, rank, w);
        let r = match self.shape(id) {
            Shape::Const(b) => {
                if b {
                    (TRUE, FALSE)
                } else {
                    (FALSE, TRUE)
                }
            }
            Shape::Trap => (FALSE, FALSE),
            Shape::Not(x) => {
                let (t, f) = self.build(bdd, x, atoms, rank, work, memo)?;
                (f, t)
            }
            Shape::Ite(c, a, b) => {
                let (tc, fc) = self.build(bdd, c, atoms, rank, work, memo)?;
                let (ta, fa) = self.build(bdd, a, atoms, rank, work, memo)?;
                let (tb, fb) = self.build(bdd, b, atoms, rank, work, memo)?;
                let x = and(bdd, tc, ta, work)?;
                let y = and(bdd, fc, tb, work)?;
                let t = bdd.apply(Op2::Or, x, y, rank, work)?;
                let x = and(bdd, tc, fa, work)?;
                let y = and(bdd, fc, fb, work)?;
                (t, bdd.apply(Op2::Or, x, y, rank, work)?)
            }
            Shape::Same(a, b, eq) => {
                let (ta, fa) = self.build(bdd, a, atoms, rank, work, memo)?;
                let (tb, fb) = self.build(bdd, b, atoms, rank, work, memo)?;
                let x = and(bdd, ta, tb, work)?;
                let y = and(bdd, fa, fb, work)?;
                let same = bdd.apply(Op2::Or, x, y, rank, work)?;
                let x = and(bdd, ta, fb, work)?;
                let y = and(bdd, fa, tb, work)?;
                let differ = bdd.apply(Op2::Or, x, y, rank, work)?;
                if eq {
                    (same, differ)
                } else {
                    (differ, same)
                }
            }
            Shape::Atom => {
                let a = &atoms[&id];
                let mut d = TRUE;
                for v in &a.defs {
                    let dv = bdd.mk(*v, FALSE, TRUE);
                    d = and(bdd, d, dv, work)?;
                }
                let (pos, neg) = match a.value {
                    Value::Const(b) => (if b { TRUE } else { FALSE }, if b { FALSE } else { TRUE }),
                    Value::Var(v, negated) => {
                        let x = bdd.mk(v, FALSE, TRUE);
                        let nx = bdd.mk(v, TRUE, FALSE);
                        if negated {
                            (nx, x)
                        } else {
                            (x, nx)
                        }
                    }
                };
                (and(bdd, d, pos, work)?, and(bdd, d, neg, work)?)
            }
        };
        memo.insert(id, r);
        Some(r)
    }

    /// The atom `id` as layer 1 or 2 sees it.
    fn atom(&mut self, id: Id, level: Level) -> Value {
        let Node::Prim(op, ty, xs) = self.node(id).clone() else {
            return Value::Var(id, false);
        };
        let cmp = matches!(op, Pr::Eq | Pr::Ne | Pr::Lt | Pr::Le | Pr::Gt | Pr::Ge);
        if !cmp || xs.len() != 2 {
            return Value::Var(id, false);
        }
        let (a, b) = (xs[0], xs[1]);
        match (level, &ty) {
            (Level::Atoms, Ty::Int { bits, signed }) => {
                self.int_atom(op, &ty, *bits, *signed, a, b)
            }
            (Level::Atoms, Ty::Str) => self.order_atom(op, &ty, a, b, true),
            (Level::Atoms, Ty::Float(_)) => self.order_atom(op, &ty, a, b, false),
            _ if op == Pr::Ne => Value::Var(self.prim(Pr::Eq, ty, vec![a, b]), true),
            _ => Value::Var(id, false),
        }
    }

    /// A comparison over a total order (`String`, bytewise) or IEEE
    /// floats: `a > b` is `b < a`, `a >= b` is `b <= a`, and `a != b` is
    /// `!(a == b)`. Over a total order `a <= b` is also `!(b < a)`, and a
    /// side compared with itself decides; over floats neither holds at NaN.
    fn order_atom(&mut self, op: Pr, ty: &Ty, a: Id, b: Id, total: bool) -> Value {
        let (op, a, b, neg) = match op {
            Pr::Gt => (Pr::Lt, b, a, false),
            Pr::Ge if total => (Pr::Lt, a, b, true),
            Pr::Ge => (Pr::Le, b, a, false),
            Pr::Le if total => (Pr::Lt, b, a, true),
            Pr::Ne => (Pr::Eq, a, b, true),
            op => (op, a, b, false),
        };
        if total && a == b {
            return Value::Const((op == Pr::Eq) != neg);
        }
        Value::Var(self.prim(op, ty.clone(), vec![a, b]), neg)
    }

    /// A comparison of two integers of `bits` bits. Arithmetic wraps, so each
    /// side is a linear form modulo `2^bits`. An equality moves every term
    /// to one side; an order comparison moves none, and against a constant
    /// takes the strict form `t < c`.
    fn int_atom(&mut self, op: Pr, ty: &Ty, bits: u8, signed: bool, a: Id, b: Id) -> Value {
        let m = mask(bits);
        let (min, max) = if signed {
            (1u64 << (bits - 1), (1u64 << (bits - 1)) - 1)
        } else {
            (0, m)
        };
        let less = |x: u64, y: u64| {
            if signed {
                sext(x, bits) < sext(y, bits)
            } else {
                x < y
            }
        };
        let la = self.lin(bits, ty, a);
        let lb = self.lin(bits, ty, b);
        let neg = |v: Value, n: bool| match v {
            Value::Const(c) => Value::Const(c != n),
            Value::Var(id, x) => Value::Var(id, x != n),
        };
        if matches!(op, Pr::Eq | Pr::Ne) {
            let d = lin_add(self, &la, &lb, m, m);
            let v = if d.0.is_empty() {
                Value::Const(d.1 == 0)
            } else {
                let nd = (
                    d.0.iter()
                        .map(|(t, k)| (*t, k.wrapping_neg() & m))
                        .collect(),
                    d.1.wrapping_neg() & m,
                );
                let x = self.intern(Node::Lin(ty.clone(), d.0, d.1));
                let y = self.intern(Node::Lin(ty.clone(), nd.0, nd.1));
                let side = if self.key(y) < self.key(x) { y } else { x };
                Value::Var(self.intern(Node::Eq0(ty.clone(), side)), false)
            };
            return neg(v, op == Pr::Ne);
        }
        let (x, y, n) = match op {
            Pr::Lt => (la, lb, false),
            Pr::Gt => (lb, la, false),
            Pr::Le => (lb, la, true),
            _ => (la, lb, true),
        };
        let v = match (x.0.is_empty(), y.0.is_empty()) {
            (true, true) => Value::Const(less(x.1, y.1)),
            (false, true) if y.1 == min => Value::Const(false),
            (true, false) if x.1 == max => Value::Const(false),
            (true, false) => {
                let c = self.intern(Node::Lin(ty.clone(), Vec::new(), x.1.wrapping_add(1) & m));
                let t = self.intern(Node::Lin(ty.clone(), y.0, y.1));
                Value::Var(
                    self.intern(Node::Prim(Pr::Lt, ty.clone(), vec![t, c])),
                    true,
                )
            }
            _ if x == y => Value::Const(false),
            _ => {
                let s = self.intern(Node::Lin(ty.clone(), x.0, x.1));
                let t = self.intern(Node::Lin(ty.clone(), y.0, y.1));
                Value::Var(
                    self.intern(Node::Prim(Pr::Lt, ty.clone(), vec![s, t])),
                    false,
                )
            }
        };
        neg(v, n)
    }

    /// `id` as a linear form modulo `2^bits` over opaque terms: a name, a
    /// field, a call, a product of two non-constants, a division, anything
    /// that is not `+`, `-` or a product by a constant of type `ty`.
    fn lin(&mut self, bits: u8, ty: &Ty, id: Id) -> (Vec<(Id, u64)>, u64) {
        let m = mask(bits);
        let node = self.node(id).clone();
        let sum = |t: &mut Table<'_>, a: Id, b: Id, k: u64| {
            let x = t.lin(bits, ty, a);
            let y = t.lin(bits, ty, b);
            lin_add(t, &x, &y, k, m)
        };
        match node {
            Node::Int(v) => (Vec::new(), v as u64 & m),
            Node::Prim(Pr::Add, t, xs) if t == *ty => sum(self, xs[0], xs[1], 1),
            Node::Prim(Pr::Sub, t, xs) if t == *ty => sum(self, xs[0], xs[1], m),
            Node::Prim(Pr::Neg, t, xs) if t == *ty => {
                let x = self.lin(bits, ty, xs[0]);
                lin_add(self, &(Vec::new(), 0), &x, m, m)
            }
            Node::Prim(Pr::Mul, t, xs) if t == *ty => {
                let x = self.lin(bits, ty, xs[0]);
                let y = self.lin(bits, ty, xs[1]);
                match (x.0.is_empty(), y.0.is_empty()) {
                    (true, _) => lin_add(self, &(Vec::new(), 0), &y, x.1, m),
                    (_, true) => lin_add(self, &(Vec::new(), 0), &x, y.1, m),
                    _ => (vec![(id, 1)], 0),
                }
            }
            _ => (vec![(id, 1)], 0),
        }
    }
}

fn mask(bits: u8) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// The residue `x` of `bits` bits, read as a signed value.
fn sext(x: u64, bits: u8) -> i64 {
    let shift = 64 - u32::from(bits);
    ((x << shift) as i64) >> shift
}

/// `a + k * b` modulo the mask `m`, the terms in key order.
fn lin_add(
    t: &mut Table<'_>,
    a: &(Vec<(Id, u64)>, u64),
    b: &(Vec<(Id, u64)>, u64),
    k: u64,
    m: u64,
) -> (Vec<(Id, u64)>, u64) {
    let mut terms: Vec<(Rc<str>, Id, u64)> = Vec::new();
    for (id, c) in
        a.0.iter()
            .map(|x| (x.0, x.1))
            .chain(b.0.iter().map(|x| (x.0, x.1.wrapping_mul(k) & m)))
    {
        match terms.iter_mut().find(|x| x.1 == id) {
            Some(x) => x.2 = x.2.wrapping_add(c) & m,
            None => terms.push((t.key(id), id, c)),
        }
    }
    terms.retain(|x| x.2 != 0);
    terms.sort();
    let c = a.1.wrapping_add(b.1.wrapping_mul(k)) & m;
    (terms.into_iter().map(|x| (x.1, x.2)).collect(), c)
}

/// The value variables of `atoms`, and every variable, definedness included.
fn vars_of(atoms: &HashMap<Id, Atom>) -> (Vec<Id>, Vec<Id>) {
    let mut vals = Vec::new();
    let mut all = Vec::new();
    for a in atoms.values() {
        if let Value::Var(v, _) = a.value {
            vals.push(v);
            all.push(v);
        }
        all.extend(&a.defs);
    }
    for xs in [&mut vals, &mut all] {
        xs.sort_by_key(|v| v.0);
        xs.dedup();
    }
    (vals, all)
}

/// The type of the value a `return` of `body` hands back.
fn returned(body: &Body) -> Option<&Type> {
    let mut todo: Vec<&St> = body.stmts.iter().collect();
    while let Some(s) = todo.pop() {
        match s {
            St::Return {
                value: Some(Val::Name(n)),
                ..
            } => return Some(&body.names[n.index()].ty),
            St::Return {
                value: Some(Val::Lit(Lit::Bool(_))),
                ..
            } => return Some(&Type::Bool),
            _ => todo.extend(s.lists().flatten()),
        }
    }
    None
}

/// The value tree of each name a walk has met.
type Env = Vec<Option<Id>>;

enum Out {
    Ret(Id),
    Fell(Env),
}

/// One walk over a body's rows. A relation walk replaces every name by its
/// value; a condition walk only the core's temporaries.
struct Walk<'t, 'd, 'b> {
    t: &'t mut Table<'d>,
    body: &'b Body,
    relation: bool,
    rows: usize,
    conds: Vec<(NodeId, Id)>,
}

/// Whether a list of rows can leave other than at its end.
fn exits(list: &[St]) -> bool {
    list.iter().any(|s| match s {
        St::Return { .. }
        | St::Trap
        | St::Break { .. }
        | St::Continue { .. }
        | St::Loop { .. }
        | St::Switch { .. } => true,
        St::If { then, els, .. } => exits(then) || exits(els),
        St::Block { body, .. } => exits(body),
        _ => false,
    })
}

impl<'b> Walk<'_, '_, 'b> {
    fn temp(&self, n: Name) -> bool {
        self.body.names[n.index()].source.starts_with('@')
    }

    fn ty(&self, n: Name) -> Ty {
        Ty::of(&self.body.names[n.index()].ty, self.t.decls)
    }

    fn val(&mut self, v: &Val, env: &Env) -> Result<Id, Unfit> {
        Ok(match v {
            Val::Lit(l) => self.t.intern(match l {
                Lit::Int(i) => Node::Int(*i),
                Lit::Byte(b) => Node::Int(i64::from(*b)),
                Lit::Float(f) => Node::Float(f.to_bits()),
                Lit::Bool(b) => Node::Bool(*b),
                Lit::Str(s) => Node::Str(s.clone()),
                Lit::Opaque(_) => Node::Opaque,
            }),
            Val::Name(n) => match env[n.index()] {
                Some(id) => id,
                None if self.relation => return Err(Unfit::Unbound),
                None => {
                    let ty = self.ty(*n);
                    self.t.intern(Node::Local(*n, ty))
                }
            },
        })
    }

    fn place(&mut self, p: &Place, env: &Env) -> Result<Id, Unfit> {
        Ok(match p {
            Place::Name(n) => self.val(&Val::Name(*n), env)?,
            Place::Global(g) => self.t.intern(Node::Global(g.clone())),
            Place::Field(b, f) => {
                let b = self.place(b, env)?;
                self.t.intern(Node::Field(b, f.clone()))
            }
            Place::Elem(b, i) => {
                let (b, i) = (self.place(b, env)?, self.val(i, env)?);
                self.t.intern(Node::Index(b, i))
            }
            Place::Key(b, k) => {
                let (b, k) = (self.place(b, env)?, self.val(k, env)?);
                self.t.intern(Node::Key(b, k))
            }
        })
    }

    fn rhs(&mut self, r: &Rhs, env: &Env) -> Result<Id, Unfit> {
        match r {
            Rhs::Val(v) => self.val(v, env),
            Rhs::Read(p) | Rhs::Take(p) => self.place(p, env),
            Rhs::Call {
                callee,
                args,
                kind,
                targets,
                ..
            } => {
                let mut xs = Vec::new();
                if let Callee::Value(n) = kind {
                    xs.push(self.val(&Val::Name(*n), env)?);
                }
                for (a, cap) in args {
                    if self.relation && (*cap == Capability::Modify || matches!(a, Arg::Place(_))) {
                        return Err(Unfit::Writes);
                    }
                    xs.push(match a {
                        Arg::Val(v) => self.val(v, env)?,
                        Arg::Place(p) => self.place(p, env)?,
                    });
                }
                for t in targets {
                    let s = match t {
                        Target::Fn(f) => f.clone(),
                        Target::Lambda(k, ..) => k.clone(),
                        Target::Param(n) => format!("param {}", n.0),
                        Target::Value(v) => format!("value {v}"),
                    };
                    xs.push(self.t.intern(Node::Static(s)));
                }
                let traps = (self.t.calls)(self.body, r);
                Ok(self.t.intern(Node::Call(callee.clone(), xs, traps)))
            }
            Rhs::Prim(op, vals, produced) => {
                let mut xs = Vec::new();
                for v in vals {
                    xs.push(self.val(v, env)?);
                }
                let ty = (vals.iter())
                    .find_map(|v| match v {
                        Val::Name(n) => Some(self.ty(*n)),
                        Val::Lit(_) => None,
                    })
                    .or_else(|| produced.as_ref().map(|t| Ty::of(t, self.t.decls)))
                    .unwrap_or(Ty::Other(String::new()));
                Ok(match op {
                    Op::Un(u) => self.t.prim(Pr::un(*u), ty, xs),
                    Op::Bin(b) => self.t.prim(Pr::bin(*b), ty, xs),
                    Op::Conv(to) => {
                        let to = Ty::of(to, self.t.decls);
                        self.t.intern(Node::Conv(to, xs[0]))
                    }
                    Op::Closure(k) => self.t.intern(Node::Closure(k.clone(), xs)),
                })
            }
            Rhs::Make(ctor, vals) => {
                let mut xs = Vec::new();
                for v in vals {
                    xs.push(self.val(v, env)?);
                }
                Ok(self.t.intern(Node::Make(format!("{ctor:?}"), xs)))
            }
        }
    }

    /// Joins the two environments after an `if` on `c`.
    fn merge(&mut self, c: Id, a: Env, b: Env) -> Env {
        (a.into_iter().zip(b))
            .map(|(x, y)| match (x, y) {
                (Some(x), Some(y)) if x == y => Some(x),
                (Some(x), Some(y)) => Some(self.t.intern(Node::Ite(c, x, y))),
                _ => None,
            })
            .collect()
    }

    /// A relation walk over `list`, the rest of the body. An `if` with a
    /// branch that leaves runs the rest once under each branch, so each path
    /// ends in its own value.
    fn run(&mut self, list: Vec<&'b St>, mut env: Env) -> Result<Out, Unfit> {
        for (i, s) in list.iter().enumerate() {
            self.rows += 1;
            if self.rows > BUDGET.nodes {
                return Err(Unfit::Budget);
            }
            let rest = &list[i + 1..];
            match s {
                St::Let(n, r) => env[n.index()] = Some(self.rhs(r, &env)?),
                St::Store {
                    place: Place::Name(n),
                    value,
                    ..
                } => env[n.index()] = Some(self.val(value, &env)?),
                St::Store { .. } => return Err(Unfit::Writes),
                St::Drop(..) | St::Row { .. } | St::Check(_) => {}
                St::Block { body, .. } => {
                    let next = body.iter().chain(rest.iter().copied()).collect();
                    return self.run(next, env);
                }
                St::If {
                    cond, then, els, ..
                } => {
                    let c = self.val(cond, &env)?;
                    if exits(then) || exits(els) {
                        let a = self.run(
                            then.iter().chain(rest.iter().copied()).collect(),
                            env.clone(),
                        )?;
                        let b = self.run(els.iter().chain(rest.iter().copied()).collect(), env)?;
                        return match (a, b) {
                            (Out::Ret(a), Out::Ret(b)) => {
                                Ok(Out::Ret(self.t.intern(Node::Ite(c, a, b))))
                            }
                            _ => Err(Unfit::NoValue),
                        };
                    }
                    let Out::Fell(a) = self.run(then.iter().collect(), env.clone())? else {
                        unreachable!("a branch with no exit falls through")
                    };
                    let Out::Fell(b) = self.run(els.iter().collect(), env)? else {
                        unreachable!("a branch with no exit falls through")
                    };
                    env = self.merge(c, a, b);
                }
                St::Return { value: Some(v), .. } => return Ok(Out::Ret(self.val(v, &env)?)),
                St::Return { value: None, .. } => return Err(Unfit::NoValue),
                St::Trap => return Ok(Out::Ret(self.t.intern(Node::Trap))),
                // A `panic` is a call for its effect, and the path traps.
                St::Do { .. } if matches!(rest.first(), Some(St::Trap)) => {}
                St::Do { .. } => return Err(Unfit::Effect),
                St::Loop { .. } | St::Break { .. } | St::Continue { .. } => {
                    return Err(Unfit::Loop)
                }
                St::Switch { .. } => return Err(Unfit::Switch),
            }
        }
        Ok(Out::Fell(env))
    }

    /// A condition walk over `list`: records each condition and answers
    /// whether the list left before its end.
    fn walk(&mut self, list: &'b [St], env: &mut Env) -> bool {
        for s in list {
            match s {
                St::Let(n, r) if self.temp(*n) => env[n.index()] = self.rhs(r, env).ok(),
                St::Store {
                    place: Place::Name(n),
                    value,
                    ..
                } if self.temp(*n) => env[n.index()] = self.val(value, env).ok(),
                St::If {
                    cond,
                    then,
                    els,
                    site,
                } => {
                    let Ok(c) = self.val(cond, env) else {
                        unreachable!("a condition walk binds every name")
                    };
                    if *site != NodeId::NONE {
                        self.conds.push((*site, c));
                    }
                    let mut a = env.clone();
                    let left_a = self.walk(then, &mut a);
                    let left_b = self.walk(els, env);
                    match (left_a, left_b) {
                        (true, _) => {}
                        (false, true) => *env = a,
                        (false, false) => {
                            let b = std::mem::take(env);
                            *env = self.merge(c, a, b);
                        }
                    }
                }
                St::Loop { body, site } => {
                    let mut inner = env.clone();
                    let exit = body.iter().position(|s| {
                        matches!(s, St::If { then, els, .. }
                            if then.is_empty() && matches!(&els[..], [St::Break { .. }]))
                    });
                    match exit {
                        Some(k) if *site != NodeId::NONE => {
                            self.walk(&body[..k], &mut inner);
                            if let St::If { cond, .. } = &body[k] {
                                let Ok(c) = self.val(cond, &inner) else {
                                    unreachable!("a condition walk binds every name")
                                };
                                self.conds.push((*site, c));
                            }
                            self.walk(&body[k + 1..], &mut inner);
                        }
                        _ => {
                            self.walk(body, &mut inner);
                        }
                    }
                }
                St::Block { body, .. } => {
                    if self.walk(body, env) {
                        return true;
                    }
                }
                St::Switch { arms, .. } => {
                    for arm in arms {
                        self.walk(&arm.body, &mut env.clone());
                    }
                }
                St::Return { .. } | St::Break { .. } | St::Continue { .. } | St::Trap => {
                    return true
                }
                St::Let(..)
                | St::Store { .. }
                | St::Drop(..)
                | St::Row { .. }
                | St::Do { .. }
                | St::Check(_) => {}
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Decl = HashMap<String, vyrn_frontend::ast::TypeDecl>;

    fn every(_: &Body, _: &Rhs) -> bool {
        true
    }

    const CMP: [Pr; 6] = [Pr::Eq, Pr::Ne, Pr::Lt, Pr::Le, Pr::Gt, Pr::Ge];

    /// A value of the brute force: an integer residue or a Bool.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum V {
        I(u64),
        B(bool),
    }

    /// `id` at `x` and `y`, parameters 0 and 1, wrapping at the type's width.
    fn eval(t: &Table<'_>, id: Id, x: u64, y: u64) -> V {
        let b = |v: V| match v {
            V::B(b) => b,
            V::I(_) => panic!("a Bool operand"),
        };
        match t.node(id) {
            Node::Param(0, _) => V::I(x),
            Node::Param(1, _) => V::I(y),
            Node::Bool(v) => V::B(*v),
            Node::Ite(c, a, e) => match b(eval(t, *c, x, y)) {
                true => eval(t, *a, x, y),
                false => eval(t, *e, x, y),
            },
            Node::Prim(Pr::Not, _, xs) => V::B(!b(eval(t, xs[0], x, y))),
            Node::Prim(op, Ty::Int { bits, signed }, xs) => {
                let m = mask(*bits);
                let at = |k: usize| match t.node(xs[k]) {
                    Node::Int(v) => *v as u64 & m,
                    _ => match eval(t, xs[k], x, y) {
                        V::I(i) => i,
                        V::B(_) => panic!("an integer operand"),
                    },
                };
                if *op == Pr::Neg {
                    return V::I(at(0).wrapping_neg() & m);
                }
                let (p, q) = (at(0), at(1));
                let lt = |p: u64, q: u64| match signed {
                    true => sext(p, *bits) < sext(q, *bits),
                    false => p < q,
                };
                match op {
                    Pr::Add => V::I(p.wrapping_add(q) & m),
                    Pr::Sub => V::I(p.wrapping_sub(q) & m),
                    Pr::Mul => V::I(p.wrapping_mul(q) & m),
                    Pr::Eq => V::B(p == q),
                    Pr::Ne => V::B(p != q),
                    Pr::Lt => V::B(lt(p, q)),
                    Pr::Le => V::B(!lt(q, p)),
                    Pr::Gt => V::B(lt(q, p)),
                    Pr::Ge => V::B(!lt(p, q)),
                    op => panic!("no brute force of {op:?}"),
                }
            }
            n => panic!("no brute force of {n:?}"),
        }
    }

    /// Every value of the type, as residues.
    fn values(bits: u8, signed: bool) -> Vec<u64> {
        let m = mask(bits);
        match signed {
            true => (-(1i64 << (bits - 1))..(1i64 << (bits - 1)))
                .map(|v| v as u64 & m)
                .collect(),
            false => (0..=m).collect(),
        }
    }

    fn table(t: &Table<'_>, id: Id, bits: u8, signed: bool) -> Vec<V> {
        let vs = values(bits, signed);
        (vs.iter())
            .flat_map(|a| vs.iter().map(move |b| (*a, *b)))
            .map(|(a, b)| eval(t, id, a, b))
            .collect()
    }

    /// Builds trees over the integer parameters `x` and `y` of one type.
    struct Ints<'t, 'd> {
        t: &'t mut Table<'d>,
        ty: Ty,
    }

    impl Ints<'_, '_> {
        fn x(&mut self) -> Id {
            self.t.intern(Node::Param(0, self.ty.clone()))
        }

        fn y(&mut self) -> Id {
            self.t.intern(Node::Param(1, self.ty.clone()))
        }

        fn k(&mut self, v: i64) -> Id {
            self.t.intern(Node::Int(v))
        }

        fn op(&mut self, op: Pr, a: Id, b: Id) -> Id {
            self.t.prim(op, self.ty.clone(), vec![a, b])
        }

        fn neg(&mut self, a: Id) -> Id {
            self.t.prim(Pr::Neg, self.ty.clone(), vec![a])
        }

        fn not(&mut self, a: Id) -> Id {
            self.t.prim(Pr::Not, Ty::Bool, vec![a])
        }

        fn id(&mut self, e: Id) -> Id {
            self.t.identity(e, &Ty::Bool, Level::Atoms).id
        }

        /// The sides of the research's `brute.py`: the two variables, the
        /// constants at the type's edges, and sums, differences and products
        /// over them.
        fn sides(&mut self, bits: u8, signed: bool) -> Vec<Id> {
            let (lo, hi) = match signed {
                true => (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1),
                false => (0, (1i64 << bits) - 1),
            };
            let mut cs = vec![0, 1, 2, lo, hi, hi - 1, lo + 1];
            cs.sort_unstable();
            cs.dedup();
            let (x, y) = (self.x(), self.y());
            let mut out = vec![x, y];
            for &c in &cs {
                out.push(self.k(c));
            }
            for &c in cs.iter().filter(|c| **c != 0) {
                let k = self.k(c);
                out.push(self.op(Pr::Add, x, k));
                out.push(self.op(Pr::Sub, x, k));
                out.push(self.op(Pr::Sub, k, x));
            }
            let (two, zero, one) = (self.k(2), self.k(0), self.k(1));
            out.push(self.op(Pr::Add, x, y));
            out.push(self.op(Pr::Sub, x, y));
            out.push(self.op(Pr::Sub, y, x));
            out.push(self.op(Pr::Mul, two, x));
            let x2 = self.op(Pr::Mul, x, two);
            out.push(self.op(Pr::Add, x2, y));
            let yx = self.op(Pr::Add, y, x);
            out.push(self.op(Pr::Add, yx, x));
            out.push(self.neg(x));
            out.push(self.op(Pr::Sub, zero, x));
            out.push(self.op(Pr::Add, y, one));
            out
        }
    }

    /// Two atoms that get one key agree on every input, at widths 3 to 6,
    /// signed and unsigned, over every comparison of two sides.
    #[test]
    fn atoms_with_one_key_agree_everywhere() {
        let decls = Decl::new();
        let mut seen = 0;
        for bits in 3..=6u8 {
            for signed in [true, false] {
                let mut t = Table::new(&decls, &every);
                let mut b = Ints {
                    t: &mut t,
                    ty: Ty::Int { bits, signed },
                };
                let sides = b.sides(bits, signed);
                let mut by_key: HashMap<Id, Vec<V>> = HashMap::new();
                let mut tables: HashMap<Vec<V>, Vec<Id>> = HashMap::new();
                for &l in &sides {
                    for &r in &sides {
                        for op in CMP {
                            let a = b.op(op, l, r);
                            let k = b.id(a);
                            let tb = table(b.t, a, bits, signed);
                            seen += 1;
                            let first = by_key.entry(k).or_insert_with(|| tb.clone());
                            assert!(*first == tb, "key {} covers two functions", b.t.key(k));
                            tables.entry(tb).or_default().push(k);
                        }
                    }
                }
                let agree: usize = tables
                    .values()
                    .map(|ks| ks.len() * (ks.len() - 1) / 2)
                    .sum();
                let mut same = 0;
                for ks in tables.values() {
                    let mut c: HashMap<Id, usize> = HashMap::new();
                    for k in ks {
                        *c.entry(*k).or_default() += 1;
                    }
                    same += c.values().map(|n| n * (n - 1) / 2).sum::<usize>();
                }
                eprintln!(
                    "width {bits} signed {signed}: {} atoms, {} keys, {} tables, \
                     {agree} agreeing pairs, {same} with one key",
                    sides.len() * sides.len() * 6,
                    by_key.len(),
                    tables.len(),
                );
            }
        }
        assert_eq!(seen, 48_288);
    }

    type Side = fn(&mut Ints<'_, '_>) -> Id;

    fn gt_xy(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        b.op(Pr::Gt, x, y)
    }

    fn lt_yx(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        b.op(Pr::Lt, y, x)
    }

    fn not_lt_xy(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        let l = b.op(Pr::Lt, x, y);
        b.not(l)
    }

    fn le_yx(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        b.op(Pr::Le, y, x)
    }

    fn le_x1(b: &mut Ints<'_, '_>) -> Id {
        let (x, k) = (b.x(), b.k(1));
        b.op(Pr::Le, x, k)
    }

    fn lt_x2(b: &mut Ints<'_, '_>) -> Id {
        let (x, k) = (b.x(), b.k(2));
        b.op(Pr::Lt, x, k)
    }

    fn succ_eq_y(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(1));
        let s = b.op(Pr::Add, x, k);
        b.op(Pr::Eq, s, y)
    }

    fn eq_pred_y(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(1));
        let s = b.op(Pr::Sub, y, k);
        b.op(Pr::Eq, x, s)
    }

    fn diff_eq_0(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(0));
        let s = b.op(Pr::Sub, x, y);
        b.op(Pr::Eq, s, k)
    }

    fn eq_xy(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        b.op(Pr::Eq, x, y)
    }

    fn regrouped(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(2));
        let m = b.op(Pr::Mul, x, k);
        let l = b.op(Pr::Add, m, y);
        let yx = b.op(Pr::Add, y, x);
        let r = b.op(Pr::Add, yx, x);
        b.op(Pr::Eq, l, r)
    }

    fn yes(b: &mut Ints<'_, '_>) -> Id {
        b.t.intern(Node::Bool(true))
    }

    fn succ_gt_x(b: &mut Ints<'_, '_>) -> Id {
        let (x, k) = (b.x(), b.k(1));
        let s = b.op(Pr::Add, x, k);
        b.op(Pr::Gt, s, x)
    }

    fn succ_lt_succ(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(1));
        let s = b.op(Pr::Add, x, k);
        let r = b.op(Pr::Add, y, k);
        b.op(Pr::Lt, s, r)
    }

    fn lt_xy(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        b.op(Pr::Lt, x, y)
    }

    fn diff_lt_0(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(0));
        let s = b.op(Pr::Sub, x, y);
        b.op(Pr::Lt, s, k)
    }

    fn twice_eq_0(b: &mut Ints<'_, '_>) -> Id {
        let (x, k, z) = (b.x(), b.k(2), b.k(0));
        let s = b.op(Pr::Mul, k, x);
        b.op(Pr::Eq, s, z)
    }

    fn eq_x0(b: &mut Ints<'_, '_>) -> Id {
        let (x, z) = (b.x(), b.k(0));
        b.op(Pr::Eq, x, z)
    }

    fn succ_gt_y(b: &mut Ints<'_, '_>) -> Id {
        let (x, y, k) = (b.x(), b.y(), b.k(1));
        let s = b.op(Pr::Add, x, k);
        b.op(Pr::Gt, s, y)
    }

    fn ge_xy(b: &mut Ints<'_, '_>) -> Id {
        let (x, y) = (b.x(), b.y());
        b.op(Pr::Ge, x, y)
    }

    /// Each rewrite layer 2 makes holds at every width and sign, and each it
    /// refuses stays two keys and differs on some input.
    #[test]
    fn the_rules_hold_and_the_refused_rewrites_stay_apart() {
        let rules: [(&str, Side, Side, bool); 11] = [
            ("x > y, y < x", gt_xy, lt_yx, true),
            ("!(x < y), y <= x", not_lt_xy, le_yx, true),
            ("x <= 1, x < 2", le_x1, lt_x2, true),
            ("x + 1 == y, x == y - 1", succ_eq_y, eq_pred_y, true),
            ("x - y == 0, x == y", diff_eq_0, eq_xy, true),
            ("x * 2 + y == y + x + x, true", regrouped, yes, true),
            ("x + 1 > x, true", succ_gt_x, yes, false),
            ("x + 1 < y + 1, x < y", succ_lt_succ, lt_xy, false),
            ("x - y < 0, x < y", diff_lt_0, lt_xy, false),
            ("2 * x == 0, x == 0", twice_eq_0, eq_x0, false),
            ("x + 1 > y, x >= y", succ_gt_y, ge_xy, false),
        ];
        let decls = Decl::new();
        for (name, l, r, holds) in rules {
            for bits in 3..=6u8 {
                for signed in [true, false] {
                    let mut t = Table::new(&decls, &every);
                    let mut b = Ints {
                        t: &mut t,
                        ty: Ty::Int { bits, signed },
                    };
                    let (l, r) = (l(&mut b), r(&mut b));
                    let same = b.id(l) == b.id(r);
                    let equal = table(b.t, l, bits, signed) == table(b.t, r, bits, signed);
                    assert_eq!(
                        (same, equal),
                        (holds, holds),
                        "{name} at {bits} bits, signed {signed}"
                    );
                }
            }
        }
    }

    /// A deterministic generator: the 64-bit LCG of Knuth's MMIX.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: usize) -> usize {
            self.0 = (self.0)
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) % n as u64) as usize
        }
    }

    /// A random formula of depth at most `depth` over `pool`: `!`, `&&`,
    /// `||` and `if`, as the core writes them.
    fn formula(t: &mut Table<'_>, pool: &[Id], depth: u32, rng: &mut Lcg) -> Id {
        if depth == 0 || rng.below(10) < 3 {
            return pool[rng.below(pool.len())];
        }
        let (tt, ff) = (t.intern(Node::Bool(true)), t.intern(Node::Bool(false)));
        let pick = rng.below(10);
        let mut sub = |t: &mut Table<'_>| formula(t, pool, depth - 1, rng);
        match pick {
            0 | 1 => {
                let a = sub(t);
                t.prim(Pr::Not, Ty::Bool, vec![a])
            }
            2..=4 => {
                let (x, y) = (sub(t), sub(t));
                t.intern(Node::Ite(x, y, ff))
            }
            5..=7 => {
                let (x, y) = (sub(t), sub(t));
                t.intern(Node::Ite(x, tt, y))
            }
            _ => {
                let (c, x, y) = (sub(t), sub(t), sub(t));
                t.intern(Node::Ite(c, x, y))
            }
        }
    }

    /// Over 4,000 random formulas of atoms at 4 bits, two with one identity
    /// agree everywhere, and each of the first 1,500 shares its identity
    /// with its rewritten twin `!!((f && true) || false)`.
    #[test]
    fn the_boolean_layer_is_sound_and_keeps_twins_together() {
        let decls = Decl::new();
        let mut t = Table::new(&decls, &every);
        let mut b = Ints {
            t: &mut t,
            ty: Ty::Int {
                bits: 4,
                signed: true,
            },
        };
        let sides = b.sides(4, true);
        let mut pool = Vec::new();
        for &l in &sides[..8] {
            for &r in &sides[..8] {
                for op in CMP {
                    pool.push(b.op(op, l, r));
                }
            }
        }
        let mut rng = Lcg(7);
        let mut keyed: HashMap<Id, Vec<V>> = HashMap::new();
        for i in 0..4000 {
            let f = formula(b.t, &pool, 4, &mut rng);
            let k = b.id(f);
            let tb = table(b.t, f, 4, true);
            assert!(
                *keyed.entry(k).or_insert_with(|| tb.clone()) == tb,
                "formula {i}"
            );
            if i < 1500 {
                let (tt, ff) = (b.t.intern(Node::Bool(true)), b.t.intern(Node::Bool(false)));
                let g = b.t.intern(Node::Ite(f, tt, ff));
                let g = b.t.intern(Node::Ite(g, tt, ff));
                let g = b.not(g);
                let g = b.not(g);
                assert_eq!(b.id(g), k, "twin of formula {i}");
            }
        }
        eprintln!("4000 formulas, {} identities", keyed.len());
    }

    /// `p_i op k` over the Int64 parameter `i`.
    fn cmp0(t: &mut Table<'_>, i: u32, op: Pr, k: i64) -> Id {
        let ty = Ty::Int {
            bits: 64,
            signed: true,
        };
        let p = t.intern(Node::Param(i, ty.clone()));
        let k = t.intern(Node::Int(k));
        t.prim(op, ty, vec![p, k])
    }

    fn and_all(t: &mut Table<'_>, xs: &[Id]) -> Id {
        let ff = t.intern(Node::Bool(false));
        (xs.iter().rev().copied())
            .reduce(|rest, c| t.intern(Node::Ite(c, rest, ff)))
            .expect("a conjunct")
    }

    fn or_all(t: &mut Table<'_>, xs: &[Id]) -> Id {
        let tt = t.intern(Node::Bool(true));
        (xs.iter().rev().copied())
            .reduce(|rest, c| t.intern(Node::Ite(c, tt, rest)))
            .expect("a disjunct")
    }

    /// Past the atom budget, a conjunction over disjoint atoms is split, so
    /// its conjuncts reorder and each normalizes on its own.
    #[test]
    fn a_wide_conjunction_splits_into_its_parts() {
        let decls = Decl::new();
        let mut t = Table::new(&decls, &every);
        let n = BUDGET.atoms as u32 + 1;
        let mut xs: Vec<Id> = (0..n).map(|i| cmp0(&mut t, i, Pr::Gt, 0)).collect();
        let a = and_all(&mut t, &xs);
        xs.reverse();
        xs[0] = cmp0(&mut t, n - 1, Pr::Ge, 1);
        let b = and_all(&mut t, &xs);
        let (ca, cb) = (t.canon(a, Level::Atoms), t.canon(b, Level::Atoms));
        assert_eq!((ca.atoms, ca.step), (17, Step::Split));
        assert_eq!(ca.id, cb.id);
        xs[0] = cmp0(&mut t, n - 1, Pr::Gt, 1);
        let c = and_all(&mut t, &xs);
        assert_ne!(t.canon(c, Level::Atoms).id, ca.id);
    }

    /// `a_0 == b_0 || .. || a_7 == b_7` over Bool parameters is past the node
    /// budget in the keys' order, which puts every `b` first, and within it
    /// with each pair adjacent, which sifting finds.
    #[test]
    fn sifting_finds_an_order_within_the_node_budget() {
        let decls = Decl::new();
        let mut t = Table::new(&decls, &every);
        let eqs: Vec<Id> = (0..8)
            .map(|i| {
                let a = t.intern(Node::Param(i, Ty::Bool));
                let b = t.intern(Node::Free(format!("b{i}"), Ty::Bool));
                t.prim(Pr::Eq, Ty::Bool, vec![a, b])
            })
            .collect();
        let f = or_all(&mut t, &eqs);
        let rev: Vec<Id> = eqs.iter().rev().copied().collect();
        let g = or_all(&mut t, &rev);
        let (cf, cg) = (t.canon(f, Level::Boolean), t.canon(g, Level::Boolean));
        assert_eq!(cf.step, Step::Sifted);
        assert!(cf.nodes <= BUDGET.nodes, "{} nodes", cf.nodes);
        assert_eq!(cf.id, cg.id);
    }

    /// Past every budget, a disjunction keeps its outer structure and decides
    /// each part that fits, so a rewritten atom inside still joins.
    #[test]
    fn past_the_budget_each_part_that_fits_is_decided() {
        let decls = Decl::new();
        let mut t = Table::new(&decls, &every);
        let n = BUDGET.atoms as u32 + 1;
        let mut xs: Vec<Id> = (0..n - 1).map(|i| cmp0(&mut t, i, Pr::Gt, 0)).collect();
        xs.push(cmp0(&mut t, n - 1, Pr::Ge, 18));
        let a = or_all(&mut t, &xs);
        *xs.last_mut().expect("a disjunct") = cmp0(&mut t, n - 1, Pr::Gt, 17);
        let b = or_all(&mut t, &xs);
        let (ca, cb) = (t.canon(a, Level::Atoms), t.canon(b, Level::Atoms));
        assert_eq!((ca.atoms, ca.step), (17, Step::Partial));
        assert_eq!(ca.id, cb.id);
        assert_ne!(t.canon(a, Level::Boolean).id, t.canon(b, Level::Boolean).id);
    }
}
