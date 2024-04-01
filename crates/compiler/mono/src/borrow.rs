use bumpalo::{collections::Vec, Bump};
use roc_collections::{MutMap, ReferenceMatrix};
use roc_error_macros::todo_lambda_erasure;
use roc_module::symbol::Symbol;

use crate::{
    inc_dec::Ownership,
    ir::{Call, CallType, Expr, Proc, ProcLayout, Stmt},
    layout::{Builtin, InLayout, LayoutInterner, LayoutRepr, Niche},
};

#[derive(Clone, Copy)]
pub(crate) struct BorrowSignature(u64);

impl std::fmt::Debug for BorrowSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut f = &mut f.debug_struct("BorrowSignature");

        dbg!(self.0);

        for (i, ownership) in self.iter().enumerate() {
            f = f.field(&format!("_{i}"), &ownership);
        }

        f.finish()
    }
}

impl BorrowSignature {
    fn new(len: usize) -> Self {
        assert!(len < 64 - 8);

        Self(len as _)
    }

    fn len(&self) -> usize {
        (self.0 & 0xFF) as usize
    }

    fn get(&self, index: usize) -> Option<&Ownership> {
        if index >= self.len() {
            return None;
        }

        match self.0 & (1 << (index + 8)) {
            0 => Some(&Ownership::Borrowed),
            _ => Some(&Ownership::Owned),
        }
    }

    fn set(&mut self, index: usize, ownership: Ownership) {
        assert!(index < self.len());

        let mask = 1 << (index + 8);

        match ownership {
            Ownership::Owned => self.0 |= mask,
            Ownership::Borrowed => self.0 &= !mask,
        }
    }

    fn iter(&self) -> impl Iterator<Item = Ownership> + '_ {
        let mut i = 0;

        std::iter::from_fn(move || {
            let value = self.get(i)?;
            i += 1;
            Some(*value)
        })
    }
}

impl std::ops::Index<usize> for BorrowSignature {
    type Output = Ownership;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).unwrap()
    }
}

pub(crate) type BorrowSignatures<'a> = MutMap<(Symbol, ProcLayout<'a>), BorrowSignature>;

pub(crate) fn infer_borrow_signatures<'a>(
    arena: &'a Bump,
    interner: &impl LayoutInterner<'a>,
    procs: &'a MutMap<(Symbol, ProcLayout<'a>), Proc<'a>>,
) -> BorrowSignatures<'a> {
    let host_exposed_procs = &[];

    let mut borrow_signatures = procs
        .iter()
        .map(|(key, proc)| {
            let mut signature = BorrowSignature::new(proc.args.len());

            for (i, in_layout) in key.1.arguments.iter().enumerate() {
                signature.set(i, layout_to_ownership(*in_layout, interner));
            }

            (*key, signature)
        })
        .collect();

    // next we first partition the functions into strongly connected components, then do a
    // topological sort on these components, finally run the fix-point borrow analysis on each
    // component (in top-sorted order, from primitives (std-lib) to main)

    let matrix = construct_reference_matrix(arena, procs);
    let sccs = matrix.strongly_connected_components_all();

    let mut env = ();

    for (group, _) in sccs.groups() {
        // This is a fixed-point analysis
        //
        // all functions initially own all their parameters
        // through a series of checks and heuristics, some arguments are set to borrowed
        // when that doesn't lead to conflicts the change is kept, otherwise it may be reverted
        //
        // when the signatures no longer change, the analysis stops and returns the signatures
        loop {
            for index in group.iter_ones() {
                let (key, proc) = procs.iter().nth(index).unwrap();

                if proc.args.is_empty() {
                    continue;
                }

                // host-exposed functions must always own their arguments.
                let is_host_exposed = host_exposed_procs.contains(&key.0);

                let mut state = State::new(arena, interner, &mut borrow_signatures, proc);
                state.inspect_stmt(&mut borrow_signatures, &proc.body);

                borrow_signatures.insert(*key, state.borrow_signature);
            }

            // if there were no modifications, we're done
            // if !env.modified {
            if true {
                break;
            }
        }
    }

    borrow_signatures
}

#[allow(unused)]
fn infer_borrow_signature<'a>(
    arena: &'a Bump,
    interner: &impl LayoutInterner<'a>,
    borrow_signatures: &'a mut BorrowSignatures<'a>,
    proc: &'a Proc<'a>,
) -> BorrowSignature {
    let mut state = State::new(arena, interner, borrow_signatures, proc);
    state.inspect_stmt(borrow_signatures, &proc.body);
    state.borrow_signature
}

struct State<'a> {
    /// Argument symbols with a layout of `List *` or `Str`, i.e. the layouts
    /// for which borrow inference might decide to pass as borrowed
    args: &'a [(InLayout<'a>, Symbol)],
    borrow_signature: BorrowSignature,
}

fn layout_to_ownership<'a>(
    in_layout: InLayout<'a>,
    interner: &impl LayoutInterner<'a>,
) -> Ownership {
    match interner.get_repr(in_layout) {
        LayoutRepr::Builtin(Builtin::Str | Builtin::List(_)) => Ownership::Borrowed,
        LayoutRepr::LambdaSet(inner) => {
            layout_to_ownership(inner.runtime_representation(), interner)
        }
        _ => Ownership::Owned,
    }
}

impl<'a> State<'a> {
    fn new(
        arena: &'a Bump,
        interner: &impl LayoutInterner<'a>,
        borrow_signatures: &mut BorrowSignatures<'a>,
        proc: &'a Proc<'a>,
    ) -> Self {
        let key = (proc.name.name(), proc.proc_layout(arena));

        // initialize the borrow signature based on the layout if first time
        let borrow_signature = borrow_signatures.entry(key).or_insert_with(|| {
            let mut borrow_signature = BorrowSignature::new(proc.args.len());

            for (i, in_layout) in key.1.arguments.iter().enumerate() {
                borrow_signature.set(i, layout_to_ownership(*in_layout, interner));
            }

            borrow_signature
        });

        Self {
            args: proc.args,
            borrow_signature: *borrow_signature,
        }
    }

    /// Mark the given argument symbol as Owned if the symbol participates in borrow inference
    ///
    /// Currently argument symbols participate if `layout_to_ownership` returns `Borrowed` for their layout.
    fn mark_owned(&mut self, symbol: Symbol) {
        if let Some(index) = self.args.iter().position(|(_, s)| *s == symbol) {
            self.borrow_signature.set(index, Ownership::Owned);
        }
    }

    fn inspect_stmt(&mut self, borrow_signatures: &mut BorrowSignatures<'a>, stmt: &'a Stmt<'a>) {
        match stmt {
            Stmt::Let(_, expr, _, stmt) => {
                self.inspect_expr(borrow_signatures, expr);
                self.inspect_stmt(borrow_signatures, stmt);
            }
            Stmt::Switch {
                branches,
                default_branch,
                ..
            } => {
                for (_, _, stmt) in branches.iter() {
                    self.inspect_stmt(borrow_signatures, stmt);
                }
                self.inspect_stmt(borrow_signatures, default_branch.1);
            }
            Stmt::Ret(s) => {
                // to return a value we must own it
                // (with the current implementation anyway)
                self.mark_owned(*s);
            }
            Stmt::Refcounting(_, _) => unreachable!("not inserted yet"),
            Stmt::Expect { .. } | Stmt::ExpectFx { .. } => {
                // TODO do we rely on values being passed by-value here?
                // it would be better to pass by-reference in general
            }
            Stmt::Dbg { .. } => {
                // TODO do we rely on values being passed by-value here?
                // it would be better to pass by-reference in general
            }
            Stmt::Join {
                body, remainder, ..
            } => {
                self.inspect_stmt(borrow_signatures, body);
                self.inspect_stmt(borrow_signatures, remainder);
            }

            Stmt::Jump(_, _) | Stmt::Crash(_, _) => { /* not relevant for ownership */ }
        }
    }

    fn inspect_expr(&mut self, borrow_signatures: &mut BorrowSignatures<'a>, expr: &'a Expr<'a>) {
        if let Expr::Call(call) = expr {
            self.inspect_call(borrow_signatures, call)
        }
    }

    fn inspect_call(&mut self, borrow_signatures: &mut BorrowSignatures<'a>, call: &'a Call<'a>) {
        let Call {
            call_type,
            arguments,
        } = call;

        match call_type {
            CallType::ByName {
                name,
                arg_layouts,
                ret_layout,
                ..
            } => {
                let proc_layout = ProcLayout {
                    arguments: arg_layouts,
                    result: *ret_layout,
                    niche: Niche::NONE,
                };

                let borrow_signature = match borrow_signatures.get(&(name.name(), proc_layout)) {
                    Some(s) => s,
                    None => todo!("no borrow signature for function/layout"),
                };

                for (argument, ownership) in arguments.iter().zip(borrow_signature.iter()) {
                    if let Ownership::Owned = ownership {
                        self.mark_owned(*argument);
                    }
                }
            }
            CallType::LowLevel { op, .. } => {
                // if the lowlevel must own the argument, mark it as owned
                let borrow_signature = crate::inc_dec::lowlevel_borrow_signature(*op);

                for (argument, ownership) in arguments.iter().zip(borrow_signature) {
                    if ownership.is_owned() {
                        self.mark_owned(*argument);
                    }
                }
            }
            CallType::ByPointer { .. } | CallType::Foreign { .. } | CallType::HigherOrder(_) => {
                for argument in arguments.iter() {
                    self.mark_owned(*argument)
                }
            }
        }
    }
}

fn construct_reference_matrix<'a>(
    arena: &'a Bump,
    procs: &MutMap<(Symbol, ProcLayout<'a>), Proc<'a>>,
) -> ReferenceMatrix {
    let mut matrix = ReferenceMatrix::new(procs.len());

    let mut call_info = CallInfo::new(arena);

    for (row, proc) in procs.values().enumerate() {
        call_info.clear();
        call_info.stmt(arena, &proc.body);

        for key in call_info.keys.iter() {
            // the same symbol can be in `keys` multiple times (with different layouts)
            for (col, (k, _)) in procs.keys().enumerate() {
                if k == key {
                    matrix.set_row_col(row, col, true);
                }
            }
        }
    }

    matrix
}

struct CallInfo<'a> {
    keys: Vec<'a, Symbol>,
}

impl<'a> CallInfo<'a> {
    fn new(arena: &'a Bump) -> Self {
        CallInfo {
            keys: Vec::new_in(arena),
        }
    }

    fn clear(&mut self) {
        self.keys.clear()
    }

    fn call(&mut self, call: &crate::ir::Call<'a>) {
        use crate::ir::CallType::*;
        use crate::ir::HigherOrderLowLevel;
        use crate::ir::PassedFunction;

        match call.call_type {
            ByName { name, .. } => {
                self.keys.push(name.name());
            }
            ByPointer { .. } => {
                todo_lambda_erasure!()
            }
            Foreign { .. } => {}
            LowLevel { .. } => {}
            HigherOrder(HigherOrderLowLevel {
                passed_function: PassedFunction { name, .. },
                ..
            }) => {
                self.keys.push(name.name());
            }
        }
    }

    fn stmt(&mut self, arena: &'a Bump, stmt: &Stmt<'a>) {
        use Stmt::*;

        let mut stack = bumpalo::vec![in arena; stmt];

        while let Some(stmt) = stack.pop() {
            match stmt {
                Join {
                    remainder: v,
                    body: b,
                    ..
                } => {
                    stack.push(v);
                    stack.push(b);
                }
                Let(_, expr, _, cont) => {
                    if let Expr::Call(call) = expr {
                        self.call(call);
                    }
                    stack.push(cont);
                }
                Switch {
                    branches,
                    default_branch,
                    ..
                } => {
                    stack.extend(branches.iter().map(|b| &b.2));
                    stack.push(default_branch.1);
                }

                Dbg { remainder, .. } => stack.push(remainder),
                Expect { remainder, .. } => stack.push(remainder),
                ExpectFx { remainder, .. } => stack.push(remainder),

                Refcounting(_, _) => unreachable!("these have not been introduced yet"),

                Ret(_) | Jump(_, _) | Crash(..) => {
                    // these are terminal, do nothing
                }
            }
        }
    }
}
