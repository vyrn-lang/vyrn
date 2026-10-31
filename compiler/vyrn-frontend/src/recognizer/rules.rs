// Written by vyrn_syntax::recognizer from compiler/vyrn-syntax/vyrn.syntax. Do
// not edit: the test the_recognizer_is_fresh fails when this file differs from a
// fresh run, and VYRN_PIN=write rewrites it.

//! The syntactic rules of `vyrn.syntax` as Rust: one method of [`Recognizer`] per
//! rule that `Module` reaches. A method returns `Ok(false)` with the cursor
//! unmoved when its rule does not match, and `Err` when the rule refuses.

use super::{Capture, Recognizer, R};
use crate::lexer::Tok;
use crate::rules::rule;

/// A rule with a node in the event log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Expr,
    Lambda,
    NsStruct,
    Tagged,
    RefLet,
    Folded,
    Fallible,
    TypeArgs,
}

/// A rule whose close the host reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    TName,
    TypeName,
    IHead,
    IBind,
    Recv,
    RCap,
    IMethod,
    StmtExpr,
    PlainStmt,
    AwaitTail,
    RefLet,
    CName,
    CLet,
    CFn,
    TypeDecl,
    LogHead,
    SkimBody,
    TestBody,
    DeclName,
    ImportName,
    Module,
    Item,
    Export,
    GenFn,
    MutFn,
    Import,
    Source,
    FnDecl,
    Params,
    Param,
    Ret,
    Type,
    Field,
}

/// The grammar's context flags.
#[derive(Clone, Copy, Default)]
pub(super) struct Flags {
    pub(super) exported: bool,
    pub(super) collect: bool,
    pub(super) nostruct: bool,
    pub(super) methods: bool,
    pub(super) scoped: bool,
    pub(super) named: bool,
    pub(super) place: bool,
}

impl Recognizer<'_> {
    /// The `expect` row for the token named `token`.
    pub(super) fn expected(&mut self, token: &str) -> R {
        let me = self;
        me.refuse((me.here().0, me.here().1), |me| rule!(Expected, pty = token, aty = me.found()))
    }

    /// The `string` token; the host parses each hole with `Expr`.
    fn string(&mut self, tagged: bool) -> R {
        self.string_token(
            tagged,
            |me| {
                if !me._expr()? {
                    me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?;
                }
                Ok(true)
            },
            |detail| rule!(InInterpolation, detail),
            |src| rule!(InterpolationTrailing, src),
        )
    }

    fn _ident(&mut self) -> R {
        let me = self;
        let hit = me.eat_if(matches!(me.peek(), Tok::Ident(_)));
        Ok(hit)
    }

    fn _close(&mut self) -> R {
        let me = self;
        let hit = me.close_gt();
        Ok(hit)
    }

    pub(super) fn _module(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me.many_recovering(
                |me| matches!(me.peek(), Tok::Eof),
                |me| Ok(me._item()?),
                |me| me.expected("\"\""),
                Some(|me: &mut Self| Ok(me._item_start()?)),
            )?;
        if hit {
            me.closed(Hook::Module, open, start)?;
        }
        Ok(hit)
    }

    fn _item_start(&mut self) -> R {
        let me = self;
        let hit = me.eat(&Tok::Fn)
            || me.eat(&Tok::Type)
            || me.eat(&Tok::Protocol)
            || me.eat(&Tok::Impl)
            || me.eat(&Tok::Import)
            || me.eat(&Tok::Export)
            || me.eat(&Tok::Let)
            || me.word("logging")
            || ({
                let p0 = me.save();
                (me.eat(&Tok::Mut)
                    && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("extern")
                    && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("gen")
                    && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("test")
                    && (me.eat_if(matches!(me.peek(), Tok::Str(_))) || (me.committed(p0) && me.expected("\"\\\"\"")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("bench")
                    && (me.eat_if(matches!(me.peek(), Tok::Str(_))) || (me.committed(p0) && me.expected("\"\\\"\"")?)))
                || { me.back(p0); false }
            })
            || me._contract_head()?;
        Ok(hit)
    }

    fn _contract_head(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.word("contract")
                && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?))
                && (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _item(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (({ loop { let p = me.cur; if !me.eat_if(matches!(me.peek(), Tok::Doc(_))) || me.cur == p { break; } } true })
                && ((
                matches!(me.peek(), Tok::Eof)
                || me._export()?
                || me._decl()?
            ) || (me.committed(p0) && me.expected("\"\"")?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Item, open, start)?;
        }
        Ok(hit)
    }

    fn _export(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Export)
                && ((
                ({
                    let p0 = me.save();
                    (me.look(|me| Ok(({
                        let p0 = me.save();
                        (me._export_to()?
                            && (me.word("extern") || (me.committed(p0) && me.expected("\"extern\"")?))
                            && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                        || { me.back(p0); false }
                    })))
                        && (me._export_to()? || (me.committed(p0) && me.expected("\"to\"")?))
                        && (me.refuse((me.here().0, me.here().1), |me| rule!(ExportToExtern))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ExportToExtern))?)))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let _ = me._export_to()?; true })
                        && (({ let old = std::mem::replace(&mut me.flags.exported, true); let hit = me._export_target()?; me.flags.exported = old; hit }) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ExportNeedsDecl))?)))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ExportNeedsDecl))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Export, open, start)?;
        }
        Ok(hit)
    }

    fn _export_to(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Ident(w) if w == "to") && matches!(me.ahead(1), Tok::Str(_)))
                && (me.word("to") || (me.committed(p0) && me.expected("\"to\"")?))
                && (me.eat_if(matches!(me.peek(), Tok::Str(_))) || (me.committed(p0) && me.expected("\"\\\"\"")?))
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Comma) && matches!(me.ahead(1), Tok::Str(_)))
                    && (me.eat(&Tok::Comma) || (me.committed(p0) && me.expected("Comma")?))
                    && (me.eat_if(matches!(me.peek(), Tok::Str(_))) || (me.committed(p0) && me.expected("\"\\\"\"")?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _export_target(&mut self) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                (me.look(|me| Ok((
                    me.eat(&Tok::Fn)
                    || me.eat(&Tok::Type)
                    || me.eat(&Tok::Protocol)
                    || ({
                        let p0 = me.save();
                        (me.word("extern")
                            && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                        || { me.back(p0); false }
                    })
                    || ({
                        let p0 = me.save();
                        (me.word("gen")
                            && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                        || { me.back(p0); false }
                    })
                    || me._contract_head()?
                    || ({
                        let p0 = me.save();
                        (me.eat(&Tok::Mut)
                            && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?)))
                        || { me.back(p0); false }
                    })
                )))
                    && (me._decl()? || (me.committed(p0) && me.expected("\"\"")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::Let)
                    && (me.refuse((me.here().0, me.here().1), |me| rule!(ModuleStateExport))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ModuleStateExport))?)))
                || { me.back(p0); false }
            })
            || me.refuse((me.here().0, me.here().1), |me| rule!(ExportNeedsDecl))?;
        Ok(hit)
    }

    fn _decl(&mut self) -> R {
        let me = self;
        let hit = me._import()?
            || me._type_decl()?
            || me._fn_decl()?
            || me._gen_fn()?
            || me._mut_fn()?
            || me._protocol()?
            || me._contract()?
            || me._impl()?
            || me._global()?
            || me._extern()?
            || me._test()?
            || me._bench()?
            || me._logging()?
            || ({ me.skip((me.here().0, me.here().1), |me| rule!(TopLevelExpected, found = me.found())); true });
        Ok(hit)
    }

    fn _import(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (({ let from = me.cur; me.eat(&Tok::Import) && ({ let l_i = Capture { from, to: me.cur };
                ((
                ({
                    let p0 = me.save();
                    (me.eat(&Tok::Star)
                        && ((
                        me.word("as")
                        || me.refuse_past((me.line_of(l_i), me.here().1), |me| rule!(ImportStarAs, found = me.found()))?
                    ) || (me.committed(p0) && me.refuse_past((me.line_of(l_i), me.here().1), |me| rule!(ImportStarAs, found = me.found()))?))
                        && ({ let from = me.cur; (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_ns = Capture { from, to: me.cur };
                        ((
                        me.word("from")
                        || me.refuse_past((me.line_of(l_i), me.here().1), |me| rule!(ImportStarFrom, ns = me.text(l_ns), found = me.found()))?
                    ) || (me.committed(p0) && me.refuse_past((me.line_of(l_i), me.here().1), |me| rule!(ImportStarFrom, ns = me.text(l_ns), found = me.found()))?))
                        && (me._source(l_i)? || (me.committed(p0) && me.refuse((me.line_of(l_i), me.here().1), |me| rule!(ImportPathExpected, found = me.found()))?)) }) }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let _ = me.eat(&Tok::Type); true })
                        && (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                        && ((
                        ({
                            let p0 = me.save();
                            (me.eat(&Tok::RBrace)
                                && (me.refuse((me.line_of(l_i), me.here().1), |me| rule!(ImportEmpty))? || (me.committed(p0) && me.refuse((me.line_of(l_i), me.here().1), |me| rule!(ImportEmpty))?)))
                            || { me.back(p0); false }
                        })
                        || ({
                            let p0 = me.save();
                            (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RBrace) { break; } if !me._import_name()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true })
                                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
                            || { me.back(p0); false }
                        })
                    ) || (me.committed(p0) && me.expected("\"\"")?))
                        && ((
                        me.word("from")
                        || me.refuse_past((me.line_of(l_i), me.here().1), |me| rule!(ImportFrom, found = me.found()))?
                    ) || (me.committed(p0) && me.refuse_past((me.line_of(l_i), me.here().1), |me| rule!(ImportFrom, found = me.found()))?))
                        && (me._source(l_i)? || (me.committed(p0) && me.refuse((me.line_of(l_i), me.here().1), |me| rule!(ImportPathExpected, found = me.found()))?)))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.expected("LBrace")?))
                && ({ let _ = me.eat(&Tok::Semi); true }) }) }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Import, open, start)?;
        }
        Ok(hit)
    }

    fn _import_name(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && ({ let _ = ({
                let p0 = me.save();
                (me.word("as")
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::ImportName, open, start)?;
        }
        Ok(hit)
    }

    fn _decl_name(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me._ident()?;
        if hit {
            me.closed(Hook::DeclName, open, start)?;
        }
        Ok(hit)
    }

    fn _source(&mut self, l_i: Capture) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me.eat_if(matches!(me.peek(), Tok::Str(_)))
            || ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(_)) && matches!(me.ahead(1), Tok::LParen))
                    && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?))
                    && (me._args()? || (me.committed(p0) && me.expected("LParen")?)))
                || { me.back(p0); false }
            })
            || me.refuse((me.line_of(l_i), me.here().1), |me| rule!(ImportPathExpected, found = me.found()))?;
        if hit {
            me.closed(Hook::Source, open, start)?;
        }
        Ok(hit)
    }

    fn _type_decl(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Type)
                && (me._decl_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Lt)
                    && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::Gt) { break; } if !me._t_name()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            }); true })
                && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                && (({ let old = std::mem::replace(&mut me.flags.collect, true); let hit = (
                me._enum_type()?
                || me._type()?
            ); me.flags.collect = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Where)
                    && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::TypeDecl, open, start)?;
        }
        Ok(hit)
    }

    fn _enum_type(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Enum)
                && (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RBrace) { break; } if !me._variant()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _variant(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::LParen)
                    && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._type()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _global(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Let)
                && ({ let _ = me.eat(&Tok::Mut); true })
                && ({ let from = me.cur; (me._decl_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_n = Capture { from, to: me.cur };
                ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Colon)
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && ((
                me.eat(&Tok::Eq)
                || me.refuse((me.here().0, me.here().1), |me| rule!(GlobalNeedsInit, name = me.text(l_n)))?
            ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(GlobalNeedsInit, name = me.text(l_n)))?))
                && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && ({ let _ = me.eat(&Tok::Semi); true }) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _fn_decl(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Fn)
                && ({ let from = me.cur; (me._decl_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_name = Capture { from, to: me.cur };
                ({ let _ = me._type_binder()?; true })
                && (me._params()? || (me.committed(p0) && me.expected("LParen")?))
                && ({ let _ = me._ret(l_name)?; true })
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Where)
                    && (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && (me._skim_body()? || (me.committed(p0) && me.expected("LBrace")?)) }) }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::FnDecl, open, start)?;
        }
        Ok(hit)
    }

    fn _skim_body(&mut self) -> R {
        let me = self;
        if let Some(r) = me.skipped(Hook::SkimBody) {
            return r;
        }
        let open = me.mark();
        let start = me.cur;
        let hit = me._block()?;
        if hit {
            me.closed(Hook::SkimBody, open, start)?;
        }
        Ok(hit)
    }

    fn _test_body(&mut self) -> R {
        let me = self;
        if let Some(r) = me.skipped(Hook::TestBody) {
            return r;
        }
        let open = me.mark();
        let start = me.cur;
        let hit = me._block()?;
        if hit {
            me.closed(Hook::TestBody, open, start)?;
        }
        Ok(hit)
    }

    fn _gen_fn(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Ident(w) if w == "gen") && matches!(me.ahead(1), Tok::Fn))
                && (me.word("gen") || (me.committed(p0) && me.expected("\"gen\"")?))
                && (me._fn_decl()? || (me.committed(p0) && me.expected("Fn")?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::GenFn, open, start)?;
        }
        Ok(hit)
    }

    fn _mut_fn(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Mut) && matches!(me.ahead(1), Tok::Fn))
                && (me.eat(&Tok::Mut) || (me.committed(p0) && me.expected("Mut")?))
                && (me._fn_decl()? || (me.committed(p0) && me.expected("Fn")?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::MutFn, open, start)?;
        }
        Ok(hit)
    }

    fn _type_binder(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Lt)
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::Gt) { break; } if !me._t_param()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _t_param(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._t_name()?
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Colon)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && ({ loop { let p = me.cur; if !({
                    let p0 = me.save();
                    (me.eat(&Tok::Plus)
                        && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                    || { me.back(p0); false }
                }) || me.cur == p { break; } } true }))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _t_name(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me._ident()?;
        if hit {
            me.closed(Hook::TName, open, start)?;
        }
        Ok(hit)
    }

    fn _params(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::LParen)
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._param()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Params, open, start)?;
        }
        Ok(hit)
    }

    fn _param(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                && ({ let _ = me._cap()?; true })
                && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Where)
                    && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Param, open, start)?;
        }
        Ok(hit)
    }

    fn _cap_word(&mut self) -> R {
        let me = self;
        let hit = me.word_if(matches!(me.peek(), Tok::Ident(w) if matches!(w.as_str(), "read" | "modify" | "consume")));
        Ok(hit)
    }

    fn _cap(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.look(|me| Ok(({
                let p0 = me.save();
                (me._cap_word()?
                    && ((
                    me.eat_if(matches!(me.peek(), Tok::Ident(_)))
                    || me.eat(&Tok::Fn)
                ) || (me.committed(p0) && me.expected("Fn")?)))
                || { me.back(p0); false }
            })))
                && (me._cap_word()? || (me.committed(p0) && me.expected("\"consume\"")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _ret(&mut self, l_name: Capture) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Arrow)
                && ((
                ({
                    let p0 = me.save();
                    ((matches!(me.peek(), Tok::Ident(w) if w == "consume") && (matches!(me.ahead(1), Tok::Ident(_)) || matches!(me.ahead(1), Tok::LParen) || matches!(me.ahead(1), Tok::Fn)))
                        && (me.refuse((me.here().0, me.here().1), |me| rule!(ConsumeResult))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ConsumeResult))?)))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (((matches!(me.peek(), Tok::Ident(w) if w == "read") || matches!(me.peek(), Tok::Ident(w) if w == "modify")) && (matches!(me.ahead(1), Tok::Ident(_)) || matches!(me.ahead(1), Tok::LParen) || matches!(me.ahead(1), Tok::Fn)))
                        && (me.refuse_past((me.here().0, me.here().1), |me| rule!(FreeFnCapability, name = me.text(l_name)))? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(FreeFnCapability, name = me.text(l_name)))?)))
                    || { me.back(p0); false }
                })
                || me._type()?
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Ret, open, start)?;
        }
        Ok(hit)
    }

    fn _extern(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Ident(w) if w == "extern") && matches!(me.ahead(1), Tok::Fn))
                && (me.word("extern") || (me.committed(p0) && me.expected("\"extern\"")?))
                && (me.eat(&Tok::Fn) || (me.committed(p0) && me.expected("Fn")?))
                && (me._decl_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && (me._params()? || (me.committed(p0) && me.expected("LParen")?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Arrow)
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && ((
                ({
                    let p0 = me.save();
                    (me.flags.exported
                        && ((
                        ({
                            let p0 = me.save();
                            (matches!(me.peek(), Tok::LBrace)
                                && (me._block()? || (me.committed(p0) && me.expected("LBrace")?)))
                            || { me.back(p0); false }
                        })
                        || me.refuse((me.here().0, me.here().1), |me| rule!(ExportedExternBody))?
                    ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ExportedExternBody))?)))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (matches!(me.peek(), Tok::LBrace)
                        && (me.refuse((me.here().0, me.here().1), |me| rule!(ExternBody))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ExternBody))?)))
                    || { me.back(p0); false }
                })
                || ({ let _ = me.eat(&Tok::Semi); true })
            ) || (me.committed(p0) && me.expected("\"\"")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _test(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Ident(w) if w == "test") && matches!(me.ahead(1), Tok::Str(_)))
                && (me.word("test") || (me.committed(p0) && me.expected("\"test\"")?))
                && (me.eat_if(matches!(me.peek(), Tok::Str(_))) || (me.committed(p0) && me.expected("\"\\\"\"")?))
                && (me._test_body()? || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _bench(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Ident(w) if w == "bench") && matches!(me.ahead(1), Tok::Str(_)))
                && (me.word("bench") || (me.committed(p0) && me.expected("\"bench\"")?))
                && (me.eat_if(matches!(me.peek(), Tok::Str(_))) || (me.committed(p0) && me.expected("\"\\\"\"")?))
                && (me._test_body()? || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _log_head(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me.word("logging");
        if hit {
            me.closed(Hook::LogHead, open, start)?;
        }
        Ok(hit)
    }

    fn _logging(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._log_head()?
                && (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RBrace) { break; } if !me._log_field()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _log_field(&mut self) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                (({ let from = me.cur; me.word("level") && ({ let l_k = Capture { from, to: me.cur };
                    (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                    && (me._log_level(l_k)? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) }) }))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (({ let from = me.cur; me.word("sink") && ({ let l_k = Capture { from, to: me.cur };
                    (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                    && (me._sink()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) }) }))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (({ let from = me.cur; me._ident()? && ({ let l_k = Capture { from, to: me.cur };
                    (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                    && (me.refuse((me.line_of(l_k), me.col_of(l_k)), |me| rule!(UnknownLoggingField, other = me.text(l_k)))? || (me.committed(p0) && me.refuse((me.line_of(l_k), me.col_of(l_k)), |me| rule!(UnknownLoggingField, other = me.text(l_k)))?)) }) }))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _log_level(&mut self, l_k: Capture) -> R {
        let me = self;
        let hit = me.word("trace")
            || me.word("debug")
            || me.word("info")
            || me.word("warn")
            || me.word("error")
            || ({
                let p0 = me.save();
                (({ let from = me.cur; me._ident()? && ({ let l_n = Capture { from, to: me.cur };
                    (me.refuse((me.line_of(l_k), me.col_of(l_k)), |me| rule!(UnknownLogLevel, name = me.text(l_n)))? || (me.committed(p0) && me.refuse((me.line_of(l_k), me.col_of(l_k)), |me| rule!(UnknownLogLevel, name = me.text(l_n)))?)) }) }))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _sink(&mut self) -> R {
        let me = self;
        let hit = me.word("stderr")
            || me.word("stdout")
            || ({
                let p0 = me.save();
                (({ let from = me.cur; me.word("file") && ({ let l_s = Capture { from, to: me.cur };
                    (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                    && ((
                    me.eat_if(matches!(me.peek(), Tok::Str(_)))
                    || me.refuse_past((me.line_of(l_s), me.col_of(l_s)), |me| rule!(FileSinkPath, found = me.found()))?
                ) || (me.committed(p0) && me.refuse_past((me.line_of(l_s), me.col_of(l_s)), |me| rule!(FileSinkPath, found = me.found()))?))
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)) }) }))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (({ let from = me.cur; me._ident()? && ({ let l_n = Capture { from, to: me.cur };
                    (me.refuse((me.line_of(l_n), me.col_of(l_n)), |me| rule!(UnknownSink, other = me.text(l_n)))? || (me.committed(p0) && me.refuse((me.line_of(l_n), me.col_of(l_n)), |me| rule!(UnknownSink, other = me.text(l_n)))?)) }) }))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _protocol(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Protocol)
                && ({ let from = me.cur; (me._decl_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_pname = Capture { from, to: me.cur };
                (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                && (({ loop { if me.look(|me| Ok(({
                    let p0 = me.save();
                    (({ loop { let p = me.cur; if !me.eat_if(matches!(me.peek(), Tok::Doc(_))) || me.cur == p { break; } } true })
                        && (!matches!(me.peek(), Tok::Type) || (me.committed(p0) && me.expected("\"\"")?)))
                    || { me.back(p0); false }
                }))) { break; } let p = me.cur; if !me._p_assoc()? { me.expected("Type")?; } if me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (({ loop { if matches!(me.peek(), Tok::RBrace) { break; } let p = me.cur; if !me._p_item(l_pname)? { me.expected("Fn")?; } if me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _p_assoc(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ loop { let p = me.cur; if !me.eat_if(matches!(me.peek(), Tok::Doc(_))) || me.cur == p { break; } } true })
                && ({ let from = me.cur; (me.eat(&Tok::Type) || (me.committed(p0) && me.expected("Type")?)) && ({ let l_t = Capture { from, to: me.cur };
                ({ let from = me.cur; (me._t_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_n = Capture { from, to: me.cur };
                ((
                ({
                    let p0 = me.save();
                    (matches!(me.peek(), Tok::Eq)
                        && (me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(AssocTypeRhs, aname = me.text(l_n)))? || (me.committed(p0) && me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(AssocTypeRhs, aname = me.text(l_n)))?)))
                    || { me.back(p0); false }
                })
                || ({ let _ = me.eat(&Tok::Semi); true })
            ) || (me.committed(p0) && me.expected("\"\"")?)) }) }) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _p_item(&mut self, l_pname: Capture) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ loop { let p = me.cur; if !me.eat_if(matches!(me.peek(), Tok::Doc(_))) || me.cur == p { break; } } true })
                && ((
                matches!(me.peek(), Tok::RBrace)
                || ({
                    let p0 = me.save();
                    (({ let from = me.cur; me.eat(&Tok::Type) && ({ let l_t = Capture { from, to: me.cur };
                        ({ let from = me.cur; (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_n = Capture { from, to: me.cur };
                        ((
                        ({
                            let p0 = me.save();
                            (matches!(me.peek(), Tok::Eq)
                                && (me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(AssocTypeRhs, aname = me.text(l_n)))? || (me.committed(p0) && me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(AssocTypeRhs, aname = me.text(l_n)))?)))
                            || { me.back(p0); false }
                        })
                        || me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(AssocTypeAfterMethods, aname = me.text(l_n), name = me.text(l_pname)))?
                    ) || (me.committed(p0) && me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(AssocTypeAfterMethods, aname = me.text(l_n), name = me.text(l_pname)))?)) }) }) }) }))
                    || { me.back(p0); false }
                })
                || me._p_method()?
            ) || (me.committed(p0) && me.expected("Fn")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _p_method(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Fn)
                && (me._decl_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                && (me._recv()? || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::Vself) || (me.committed(p0) && me.expected("Vself")?))
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Comma)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                    && ({ let _ = me._cap()?; true })
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true })
                && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Arrow)
                    && ({ let _ = me._r_cap()?; true })
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _recv(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({ let _ = me._self_cap()?; true });
        if hit {
            me.closed(Hook::Recv, open, start)?;
        }
        Ok(hit)
    }

    fn _self_cap(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.look(|me| Ok(({
                let p0 = me.save();
                (me._cap_word()?
                    && (me.eat(&Tok::Vself) || (me.committed(p0) && me.expected("Vself")?)))
                || { me.back(p0); false }
            })))
                && (me._cap_word()? || (me.committed(p0) && me.expected("\"consume\"")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _r_cap(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(w) if w == "consume") && (matches!(me.ahead(1), Tok::Ident(_)) || matches!(me.ahead(1), Tok::LParen) || matches!(me.ahead(1), Tok::Fn)))
                    && (me.refuse((me.here().0, me.here().1), |me| rule!(ConsumeResult))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ConsumeResult))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (((matches!(me.peek(), Tok::Ident(w) if w == "read") || matches!(me.peek(), Tok::Ident(w) if w == "modify")) && (matches!(me.ahead(1), Tok::Ident(_)) || matches!(me.ahead(1), Tok::LParen) || matches!(me.ahead(1), Tok::Fn)))
                    && ((
                    me.word("read")
                    || me.word("modify")
                ) || (me.committed(p0) && me.expected("\"modify\"")?)))
                || { me.back(p0); false }
            });
        if hit {
            me.closed(Hook::RCap, open, start)?;
        }
        Ok(hit)
    }

    fn _contract(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.look(|me| Ok(me._contract_head()?))
                && (me.word("contract") || (me.committed(p0) && me.expected("\"contract\"")?))
                && ({ let from = me.cur; (me._c_name()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_cname = Capture { from, to: me.cur };
                (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                && (({ loop { if (matches!(me.peek(), Tok::RBrace) || matches!(me.peek(), Tok::Eof)) { break; } let p = me.cur; if !me._c_item(l_cname)? { me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberExpected, name = me.text(l_cname), found = me.found()))?; } if me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _c_name(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me._ident()?;
        if hit {
            me.closed(Hook::CName, open, start)?;
        }
        Ok(hit)
    }

    fn _c_item(&mut self, l_cname: Capture) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ loop { let p = me.cur; if !me.eat_if(matches!(me.peek(), Tok::Doc(_))) || me.cur == p { break; } } true })
                && ((
                matches!(me.peek(), Tok::RBrace)
                || me._c_let()?
                || me._c_fn()?
                || me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberExpected, name = me.text(l_cname), found = me.found()))?
            ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberExpected, name = me.text(l_cname), found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _c_let(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Let)
                && ({ let from = me.cur; (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_m = Capture { from, to: me.cur };
                ((
                me.eat(&Tok::Colon)
                || me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberNeedsType, name = me.text(l_m)))?
            ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberNeedsType, name = me.text(l_m)))?))
                && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Eq)
                    && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && ({ let _ = me.eat(&Tok::Semi); true }) }) }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::CLet, open, start)?;
        }
        Ok(hit)
    }

    fn _c_fn(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Fn)
                && ((
                ({
                    let p0 = me.save();
                    (me.eat(&Tok::Star)
                        && (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                        && ((
                        ({
                            let p0 = me.save();
                            ((matches!(me.peek(), Tok::Dot) && matches!(me.ahead(1), Tok::Dot))
                                && (me.eat(&Tok::Dot) || (me.committed(p0) && me.expected("Dot")?))
                                && (me.eat(&Tok::Dot) || (me.committed(p0) && me.expected("Dot")?)))
                            || { me.back(p0); false }
                        })
                        || ({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._c_param()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true })
                    ) || (me.committed(p0) && me.expected("\"\"")?))
                        && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                        && ({ let _ = ({
                        let p0 = me.save();
                        (me.eat(&Tok::Arrow)
                            && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                        || { me.back(p0); false }
                    }); true })
                        && ({ let _ = ({
                        let p0 = me.save();
                        (matches!(me.peek(), Tok::Eq)
                            && (me.refuse((me.here().0, me.here().1), |me| rule!(ContractOpenRuleDefault))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ContractOpenRuleDefault))?)))
                        || { me.back(p0); false }
                    }); true })
                        && ({ let _ = me.eat(&Tok::Semi); true }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let from = me.cur; me._ident()? && ({ let l_m = Capture { from, to: me.cur };
                        (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                        && ((
                        ({
                            let p0 = me.save();
                            ((matches!(me.peek(), Tok::Dot) && matches!(me.ahead(1), Tok::Dot))
                                && (me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberParams, name = me.text(l_m)))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ContractMemberParams, name = me.text(l_m)))?)))
                            || { me.back(p0); false }
                        })
                        || ({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._c_param()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true })
                    ) || (me.committed(p0) && me.expected("\"\"")?))
                        && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                        && ({ let _ = ({
                        let p0 = me.save();
                        (me.eat(&Tok::Arrow)
                            && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                        || { me.back(p0); false }
                    }); true })
                        && ({ let _ = ({
                        let p0 = me.save();
                        (me.eat(&Tok::Eq)
                            && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                        || { me.back(p0); false }
                    }); true })
                        && ({ let _ = me.eat(&Tok::Semi); true }) }) }))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::CFn, open, start)?;
        }
        Ok(hit)
    }

    fn _c_param(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _impl(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Impl)
                && ({ let _ = me._type_binder()?; true })
                && ({ let from = me.cur; (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_proto = Capture { from, to: me.cur };
                (me.eat(&Tok::For) || (me.committed(p0) && me.expected("For")?))
                && ({ let from = me.cur; (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_ty = Capture { from, to: me.cur };
                (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                && (({ let old = std::mem::replace(&mut me.flags.methods, false); let hit = ({ loop { if matches!(me.peek(), Tok::RBrace) { break; } let p = me.cur; if !me._i_item(l_proto, l_ty)? { me.expected("Fn")?; } if me.cur == p { break; } } true }); me.flags.methods = old; hit }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)) }) }) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _i_bind(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me._i_head()?
                && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::IBind, open, start)?;
        }
        Ok(hit)
    }

    fn _i_head(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Type)
                && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::IHead, open, start)?;
        }
        Ok(hit)
    }

    fn _i_item(&mut self, l_proto: Capture, l_ty: Capture) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ loop { let p = me.cur; if !me.eat_if(matches!(me.peek(), Tok::Doc(_))) || me.cur == p { break; } } true })
                && ((
                matches!(me.peek(), Tok::RBrace)
                || ({
                    let p0 = me.save();
                    (me.flags.methods
                        && ({ let from = me.cur; (me.eat(&Tok::Type) || (me.committed(p0) && me.expected("Type")?)) && ({ let l_t = Capture { from, to: me.cur };
                        ({ let from = me.cur; (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) && ({ let l_n = Capture { from, to: me.cur };
                        (me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(ImplAssocTypeOrder, aname = me.text(l_n), protocol = me.text(l_proto), ty = me.text(l_ty)))? || (me.committed(p0) && me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(ImplAssocTypeOrder, aname = me.text(l_n), protocol = me.text(l_proto), ty = me.text(l_ty)))?)) }) }) }) }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (me._i_bind()?
                        && ({ let _ = me.eat(&Tok::Semi); true }))
                    || { me.back(p0); false }
                })
                || me._i_method()?
            ) || (me.committed(p0) && me.expected("Fn")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _i_method(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Fn)
                && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                && (me._recv()? || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::Vself) || (me.committed(p0) && me.expected("Vself")?))
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Comma)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                    && ({ let _ = me._cap()?; true })
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true })
                && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Arrow)
                    && ({ let _ = me._r_cap()?; true })
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }); true })
                && (me._block()? || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::IMethod, open, start)?;
        }
        Ok(hit)
    }

    pub(super) fn _type(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me._inter()?
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Pipe)
                    && (me._inter()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true }))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Type, open, start)?;
        }
        Ok(hit)
    }

    fn _inter(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._type_atom()?
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Amp)
                    && (me._type_atom()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _type_atom(&mut self) -> R {
        let me = self;
        if me.depth >= 1024 {
            return me.refuse((me.here().0, me.here().1), |me| rule!(NestingTooDeep, max = "1024"));
        }
        me.depth += 1;
        let hit = me._record_type()?
            || me._fn_type()?
            || me.eat_if(matches!(me.peek(), Tok::Int(0..)))
            || me._dotted_type()?
            || me._named_type()?;
        me.depth -= 1;
        Ok(hit)
    }

    fn _fn_type(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Fn)
                && (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._type()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Arrow)
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _dotted_type(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            ((matches!(me.peek(), Tok::Ident(_)) && matches!(me.ahead(1), Tok::Dot))
                && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?))
                && ((({
                let p0 = me.save();
                (me.eat(&Tok::Dot)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Dot)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true })) || (me.committed(p0) && me.expected("Dot")?))
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::Lt)
                    && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::Gt) { break; } if !me._type()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _named_type(&mut self) -> R {
        let me = self;
        let hit = me._primitive()?
            || ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::Ident(w) if matches!(w.as_str(), "Stream" | "Array" | "SmallArray" | "Map" | "Option" | "Result" | "Omit" | "Pick" | "Merge" | "Partial" | "Readonly"))
                    && (me._constructor()? || (me.committed(p0) && me.expected("\"Readonly\"")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me._type_name()?
                    && ({ let _ = (
                    me.flags.scoped
                    || ({
                        let p0 = me.save();
                        (me.eat(&Tok::Lt)
                            && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::Gt) { break; } if !me._type()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                            && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                        || { me.back(p0); false }
                    })
                ); true }))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _primitive(&mut self) -> R {
        let me = self;
        let hit = me.word_if(matches!(me.peek(), Tok::Ident(w) if matches!(w.as_str(), "Int64" | "Int8" | "Int16" | "Int32" | "UInt8" | "UInt16" | "UInt32" | "UInt64" | "Float64" | "Float32" | "F32x4" | "I32x4" | "Mask32x4" | "F64x2" | "Mask64x2" | "Bool" | "String" | "Unit" | "Logger")));
        Ok(hit)
    }

    fn _constructor(&mut self) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                (me.word("Stream")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Array")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && ({ let _ = ({
                    let p0 = me.save();
                    (me.eat(&Tok::Comma)
                        && ((
                        me.eat_if(matches!(me.peek(), Tok::Int(0..)))
                        || me.refuse((me.here().0, me.here().1), |me| rule!(ArraySize))?
                    ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(ArraySize))?)))
                    || { me.back(p0); false }
                }); true })
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("SmallArray")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && ((
                    me.eat(&Tok::Comma)
                    || me.refuse((me.here().0, me.here().1), |me| rule!(SmallArrayNeedsCapacity))?
                ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(SmallArrayNeedsCapacity))?))
                    && ((
                    me.eat_if(matches!(me.peek(), Tok::Int(0..)))
                    || me.refuse((me.here().0, me.here().1), |me| rule!(SmallArrayCapacityType))?
                ) || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(SmallArrayCapacityType))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Map")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me.eat(&Tok::Comma) || (me.committed(p0) && me.expected("Comma")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Option")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Result")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me.eat(&Tok::Comma) || (me.committed(p0) && me.expected("Comma")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (({ let from = me.cur; (
                    me.word("Omit")
                    || me.word("Pick")
                ) && ({ let l_n = Capture { from, to: me.cur };
                    (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && ((
                    ({
                        let p0 = me.save();
                        ((({
                            let p0 = me.save();
                            (me.eat(&Tok::Comma)
                                && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                            || { me.back(p0); false }
                        }) && ({ loop { let p = me.cur; if !({
                            let p0 = me.save();
                            (me.eat(&Tok::Comma)
                                && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                            || { me.back(p0); false }
                        }) || me.cur == p { break; } } true }))
                            && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                        || { me.back(p0); false }
                    })
                    || ({
                        let p0 = me.save();
                        (me._close()?
                            && (me.refuse((me.here().0, me.here().1), |me| rule!(NeedsField, name = me.text(l_n)))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(NeedsField, name = me.text(l_n)))?)))
                        || { me.back(p0); false }
                    })
                ) || (me.committed(p0) && me.expected("Gt")?)) }) }))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Merge")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me.eat(&Tok::Comma) || (me.committed(p0) && me.expected("Comma")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Partial")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.word("Readonly")
                    && (me.eat(&Tok::Lt) || (me.committed(p0) && me.expected("Lt")?))
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && (me._close()? || (me.committed(p0) && me.expected("Gt")?)))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _type_name(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me._ident()?;
        if hit {
            me.closed(Hook::TypeName, open, start)?;
        }
        Ok(hit)
    }

    fn _record_type(&mut self) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                (me.flags.collect
                    && (({ let old = std::mem::replace(&mut me.flags.collect, false); let hit = ({ let old = std::mem::replace(&mut me.flags.named, true); let hit = me._record_body()?; me.flags.named = old; hit }); me.flags.collect = old; hit }) || (me.committed(p0) && me.expected("LBrace")?)))
                || { me.back(p0); false }
            })
            || ({ let old = std::mem::replace(&mut me.flags.named, false); let hit = me._record_body()?; me.flags.named = old; hit });
        Ok(hit)
    }

    fn _record_body(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::LBrace)
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RBrace) { break; } if !me._field()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _field(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                && ((
                ({
                    let p0 = me.save();
                    (({ let from = me.cur; me.word("lazy") && ({ let l_l = Capture { from, to: me.cur };
                        ((
                        me.flags.named
                        || me.refuse((me.line_of(l_l), me.col_of(l_l)), |me| rule!(LazyAnonymous))?
                    ) || (me.committed(p0) && me.refuse((me.line_of(l_l), me.col_of(l_l)), |me| rule!(LazyAnonymous))?))
                        && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                        && ({ let _ = ({
                        let p0 = me.save();
                        (({ let from = me.cur; me.eat(&Tok::Where) && ({ let l_w = Capture { from, to: me.cur };
                            (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                            && ((
                            me.flags.named
                            || me.refuse((me.line_of(l_w), me.col_of(l_w)), |me| rule!(WhereAnonymous))?
                        ) || (me.committed(p0) && me.refuse((me.line_of(l_w), me.col_of(l_w)), |me| rule!(WhereAnonymous))?))
                            && (me.refuse((me.line_of(l_w), me.col_of(l_w)), |me| rule!(LazyWhere))? || (me.committed(p0) && me.refuse((me.line_of(l_w), me.col_of(l_w)), |me| rule!(LazyWhere))?)) }) }))
                        || { me.back(p0); false }
                    }); true }) }) }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (me._type()?
                        && ({ let _ = ({
                        let p0 = me.save();
                        (({ let from = me.cur; me.eat(&Tok::Where) && ({ let l_w = Capture { from, to: me.cur };
                            (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                            && ((
                            me.flags.named
                            || me.refuse((me.line_of(l_w), me.col_of(l_w)), |me| rule!(WhereAnonymous))?
                        ) || (me.committed(p0) && me.refuse((me.line_of(l_w), me.col_of(l_w)), |me| rule!(WhereAnonymous))?)) }) }))
                        || { me.back(p0); false }
                    }); true }))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::Field, open, start)?;
        }
        Ok(hit)
    }

    pub(super) fn _block(&mut self) -> R {
        let me = self;
        if me.depth >= 1024 {
            return me.refuse((me.here().0, me.here().1), |me| rule!(NestingTooDeep, max = "1024"));
        }
        me.depth += 1;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::LBrace)
                && (me.many_recovering(
                    |me| (matches!(me.peek(), Tok::RBrace) || matches!(me.peek(), Tok::Eof)),
                    |me| Ok(me._b_item()?),
                    |me| me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found())),
                    None::<fn(&mut Self) -> R>,
                )? || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
            || { me.back(p0); false }
        });
        me.depth -= 1;
        Ok(hit)
    }

    fn _b_item(&mut self) -> R {
        let me = self;
        let hit = me.eat_if(matches!(me.peek(), Tok::Doc(_)))
            || me.eat(&Tok::Semi)
            || me._stmt()?;
        Ok(hit)
    }

    fn _stmt(&mut self) -> R {
        let me = self;
        let hit = me._let()?
            || me._return()?
            || me._break()?
            || me._continue()?
            || me._if()?
            || me._while()?
            || me._for()?
            || me._region()?
            || me._drop()?
            || me._assign()?
            || me._expr_stmt()?;
        Ok(hit)
    }

    fn _let(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ let from = me.cur; me.eat(&Tok::Let) && ({ let l_l = Capture { from, to: me.cur };
                ((
                ({
                    let p0 = me.save();
                    ((matches!(me.peek(), Tok::Mut) && matches!(me.ahead(1), Tok::Ident(_)) && matches!(me.ahead(2), Tok::LParen))
                        && (me.eat(&Tok::Mut) || (me.committed(p0) && me.expected("Mut")?))
                        && ({ let from = me.cur; (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?)) && ({ let l_v = Capture { from, to: me.cur };
                        (me.refuse((me.line_of(l_l), me.col_of(l_v)), |me| rule!(LetMutPattern, variant = me.text(l_v)))? || (me.committed(p0) && me.refuse((me.line_of(l_l), me.col_of(l_v)), |me| rule!(LetMutPattern, variant = me.text(l_v)))?)) }) }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    ((matches!(me.peek(), Tok::Ident(_)) && matches!(me.ahead(1), Tok::LParen))
                        && (me._ref_let()? || (me.committed(p0) && me.expected("\"_\"")?)))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let _ = me.eat(&Tok::Mut); true })
                        && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                        && ({ let _ = ({
                        let p0 = me.save();
                        (me.eat(&Tok::Colon)
                            && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                            && ({ let _ = ({
                            let p0 = me.save();
                            (me.eat(&Tok::Where)
                                && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                            || { me.back(p0); false }
                        }); true }))
                        || { me.back(p0); false }
                    }); true })
                        && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                        && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                        && ({ let _ = me.eat(&Tok::Semi); true }))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _ref_let(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::RefLet);
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.eat_if(matches!(me.peek(), Tok::Ident(_)))
                && (me.eat(&Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._ident()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        me.close(open, hit);
        if hit {
            me.closed(Hook::RefLet, open, start)?;
        }
        Ok(hit)
    }

    fn _return(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Return)
                && ((
                (matches!(me.peek(), Tok::Semi) || matches!(me.peek(), Tok::RBrace) || matches!(me.peek(), Tok::Eof))
                || me._expr()?
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _break(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Break)
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _continue(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Continue)
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _if(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::If)
                && ((
                ({
                    let p0 = me.save();
                    (me.eat(&Tok::Let)
                        && (me._pattern()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                        && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                        && (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                        && (me._block()? || (me.committed(p0) && me.expected("LBrace")?))
                        && ({ let _ = me._else()?; true }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit })
                        && (me._block()? || (me.committed(p0) && me.expected("LBrace")?))
                        && ({ let _ = me._else()?; true }))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _else(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Else)
                && ((
                ({
                    let p0 = me.save();
                    (me.eat(&Tok::If)
                        && ((
                        ({
                            let p0 = me.save();
                            (me.eat(&Tok::Let)
                                && (me._pattern()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                                && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                                && (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                                && (me._block()? || (me.committed(p0) && me.expected("LBrace")?))
                                && ({ let _ = me._else()?; true }))
                            || { me.back(p0); false }
                        })
                        || ({
                            let p0 = me.save();
                            (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit })
                                && (me._block()? || (me.committed(p0) && me.expected("LBrace")?))
                                && ({ let _ = me._else()?; true }))
                            || { me.back(p0); false }
                        })
                    ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                    || { me.back(p0); false }
                })
                || me._block()?
            ) || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _while(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::While)
                && ((
                ({
                    let p0 = me.save();
                    (me.eat(&Tok::Let)
                        && (me._pattern()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                        && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                        && (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                        && (me._block()? || (me.committed(p0) && me.expected("LBrace")?)))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit })
                        && (me._block()? || (me.committed(p0) && me.expected("LBrace")?)))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _for(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::For)
                && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && (me.eat(&Tok::In) || (me.committed(p0) && me.expected("In")?))
                && ({ let _ = ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(w) if w == "consume") && matches!(me.ahead(1), Tok::Ident(_)))
                    && (me.word("consume") || (me.committed(p0) && me.expected("\"consume\"")?)))
                || { me.back(p0); false }
            }); true })
                && (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && (me._block()? || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _region(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Region)
                && (me._block()? || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _drop(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Drop)
                && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _assign(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (((matches!(me.peek(), Tok::Ident(_)) || matches!(me.peek(), Tok::Vself)) && matches!(me.ahead(1), Tok::Eq))
                && ((
                me._ident()?
                || me.eat(&Tok::Vself)
            ) || (me.committed(p0) && me.expected("Vself")?))
                && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && ({ let _ = me.eat(&Tok::Semi); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _expr_stmt(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._stmt_expr()?
                && ((
                ({
                    let p0 = me.save();
                    (me.flags.place
                        && (me.eat(&Tok::Eq) || (me.committed(p0) && me.expected("Eq")?))
                        && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                        && ({ let _ = me.eat(&Tok::Semi); true }))
                    || { me.back(p0); false }
                })
                || me._plain_stmt()?
            ) || (me.committed(p0) && me.expected("\"\"")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _plain_stmt(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({ let _ = me.eat(&Tok::Semi); true });
        if hit {
            me.closed(Hook::PlainStmt, open, start)?;
        }
        Ok(hit)
    }

    fn _stmt_expr(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = me._expr()?;
        if hit {
            me.closed(Hook::StmtExpr, open, start)?;
        }
        Ok(hit)
    }

    pub(super) fn _expr(&mut self) -> R {
        self._expr_from(0)
    }

    fn _expr_from(&mut self, min: usize) -> R {
        let me = self;
        let open = me.open(Kind::Expr);
        if !me._unary()? {
            me.close(open, false);
            return Ok(false);
        }
        while let Some((level, right)) = Self::_expr_level(me.peek()) {
            if level < min {
                break;
            }
            me.take();
            if !me._expr_from(if right { level } else { level + 1 })? {
                me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?;
            }
        }
        me.close(open, true);
        Ok(true)
    }

    /// The level of a binary operator, loosest first, and whether it groups to
    /// the right.
    fn _expr_level(tok: &Tok) -> Option<(usize, bool)> {
        Some(match tok {
            Tok::OrOr => (0, false),
            Tok::AndAnd => (1, false),
            Tok::EqEq => (2, false),
            Tok::NotEq => (2, false),
            Tok::TildeMatch => (2, false),
            Tok::Lt => (3, false),
            Tok::LtEq => (3, false),
            Tok::Gt => (3, false),
            Tok::GtEq => (3, false),
            Tok::QuestionQuestion => (4, true),
            Tok::Pipe => (5, false),
            Tok::Caret => (6, false),
            Tok::Amp => (7, false),
            Tok::Shl => (8, false),
            Tok::Shr => (8, false),
            Tok::Plus => (9, false),
            Tok::Minus => (9, false),
            Tok::Star => (10, false),
            Tok::Slash => (10, false),
            Tok::Percent => (10, false),
            _ => return None,
        })
    }

    fn _unary(&mut self) -> R {
        let me = self;
        if me.depth >= 1024 {
            return me.refuse((me.here().0, me.here().1), |me| rule!(NestingTooDeep, max = "1024"));
        }
        me.depth += 1;
        let hit = ({
                let p0 = me.save();
                (me.eat(&Tok::Minus)
                    && (me._unary()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.eat(&Tok::Bang)
                    && (me._unary()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.eat(&Tok::Tilde)
                    && (me._unary()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(w) if w == "consume") && (matches!(me.ahead(1), Tok::Ident(_)) || matches!(me.ahead(1), Tok::Vself)))
                    && (me.word("consume") || (me.committed(p0) && me.expected("\"consume\"")?))
                    && (me._postfix()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(w) if w == "await") && (matches!(me.ahead(1), Tok::Ident(_)) || matches!(me.ahead(1), Tok::Vself)))
                    && (me._await_tail()? || (me.committed(p0) && me.expected("\"await\"")?)))
                || { me.back(p0); false }
            })
            || me._postfix()?;
        me.depth -= 1;
        Ok(hit)
    }

    fn _await_tail(&mut self) -> R {
        let me = self;
        let open = me.mark();
        let start = me.cur;
        let hit = ({
            let p0 = me.save();
            (me.word("await")
                && (me._postfix()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        if hit {
            me.closed(Hook::AwaitTail, open, start)?;
        }
        Ok(hit)
    }

    fn _postfix(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._primary()?
                && (({ let mut links = 0u32; loop { if !(matches!(me.peek(), Tok::Question) || matches!(me.peek(), Tok::Dot) || matches!(me.peek(), Tok::LBracket)) { break; } links += 1; if me.depth.saturating_add(links) >= 1024 { me.refuse((me.here().0, me.here().1), |me| rule!(NestingTooDeep, max = "1024"))?; } let p = me.cur; if !me._link()? { me.expected("LBracket")?; } if me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _link(&mut self) -> R {
        let me = self;
        let hit = me.eat(&Tok::Question)
            || ({
                let p0 = me.save();
                (me.eat(&Tok::Dot)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                    && ({ let _ = me._args()?; true }))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.eat(&Tok::LBracket)
                    && (({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                    && (me.eat(&Tok::RBracket) || (me.committed(p0) && me.expected("RBracket")?)))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _args(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::LParen)
                && (({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = ({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._expr()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }); me.flags.nostruct = old; hit }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _primary(&mut self) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(_)) && matches!(me.ahead(1), Tok::Arrow))
                    && (me._lambda()? || (me.committed(p0) && me.expected("LParen")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (!me.flags.nostruct
                    && ((matches!(me.peek(), Tok::Ident(_)) && matches!(me.ahead(1), Tok::Dot) && matches!(me.ahead(2), Tok::Ident(_)) && matches!(me.ahead(3), Tok::LBrace)) || (me.committed(p0) && me.expected("\"_\"")?))
                    && (me._ns_struct()? || (me.committed(p0) && me.expected("LParen")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Ident(_)) && me.no_newline_at(1) && matches!(me.ahead(1), Tok::Str(_) | Tok::TemplateStr { .. }))
                    && ((
                    ({
                        let p0 = me.save();
                        (me.word("vyrn")
                            && (me.string(true)? || (me.committed(p0) && me.expected("\"\\\"\"")?)))
                        || { me.back(p0); false }
                    })
                    || ({
                        let p0 = me.save();
                        ((matches!(me.peek(), Tok::Ident(_)) && matches!(me.ahead(1), Tok::Str(_)))
                            && ({ let from = me.cur; (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?)) && ({ let l_t = Capture { from, to: me.cur };
                            (me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(TemplateNoHole, name = me.text(l_t)))? || (me.committed(p0) && me.refuse((me.line_of(l_t), me.col_of(l_t)), |me| rule!(TemplateNoHole, name = me.text(l_t)))?)) }) }))
                        || { me.back(p0); false }
                    })
                    || me._tagged()?
                ) || (me.committed(p0) && me.expected("\"_\"")?)))
                || { me.back(p0); false }
            })
            || me._ident_expr()?
            || me.eat_if(matches!(me.peek(), Tok::Int(_) | Tok::Float(_)))
            || me.eat_if(matches!(me.peek(), Tok::Byte(_)))
            || me.string(false)?
            || me.eat(&Tok::True)
            || me.eat(&Tok::False)
            || me._array_lit()?
            || me._match()?
            || me._if_expr()?
            || ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::LParen)
                    && (me.look(|me| Ok(me._lambda_head()?)) || (me.committed(p0) && me.expected("LParen")?))
                    && (me._lambda()? || (me.committed(p0) && me.expected("LParen")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::OrOr)
                    && (me.refuse((me.here().0, me.here().1), |me| rule!(LambdaNotHere, form = "||", fix = "`() -> ...`"))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(LambdaNotHere, form = "||", fix = "`() -> ...`"))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::Pipe)
                    && (me.refuse((me.here().0, me.here().1), |me| rule!(LambdaNotHere, form = "|x|", fix = "`x -> ...`, or `(x, y) -> ...` for more than one"))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(LambdaNotHere, form = "|x|", fix = "`x -> ...`, or `(x, y) -> ...` for more than one"))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (!me.flags.nostruct
                    && ((matches!(me.peek(), Tok::Vself) && matches!(me.ahead(1), Tok::Dot) && matches!(me.ahead(2), Tok::Ident(_)) && matches!(me.ahead(3), Tok::LBrace)) || (me.committed(p0) && me.expected("Vself")?))
                    && (me._ns_struct()? || (me.committed(p0) && me.expected("LParen")?)))
                || { me.back(p0); false }
            })
            || me.eat(&Tok::Vself)
            || ({
                let p0 = me.save();
                (!me.flags.nostruct
                    && (matches!(me.peek(), Tok::LParen) || (me.committed(p0) && me.expected("LParen")?))
                    && (me.look(|me| Ok(({
                    let p0 = me.save();
                    (me._var_head()?
                        && (me.eat(&Tok::Dot) || (me.committed(p0) && me.expected("Dot")?))
                        && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?))
                        && (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?)))
                    || { me.back(p0); false }
                }))) || (me.committed(p0) && me.expected("LParen")?))
                    && (me._ns_struct()? || (me.committed(p0) && me.expected("LParen")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.eat(&Tok::LParen)
                    && (({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _ns_struct(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::NsStruct);
        let hit = ({
            let p0 = me.save();
            (me._var_head()?
                && (me.eat(&Tok::Dot) || (me.committed(p0) && me.expected("Dot")?))
                && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?))
                && (me._struct_body()? || (me.committed(p0) && me.expected("LBrace")?)))
            || { me.back(p0); false }
        });
        me.close(open, hit);
        Ok(hit)
    }

    fn _var_head(&mut self) -> R {
        let me = self;
        let hit = me.eat_if(matches!(me.peek(), Tok::Ident(_)))
            || me.eat(&Tok::Vself)
            || ({
                let p0 = me.save();
                (me.eat(&Tok::LParen)
                    && (me._var_head()? || (me.committed(p0) && me.expected("LParen")?))
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _tagged(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::Tagged);
        let hit = ({
            let p0 = me.save();
            (me.eat_if(matches!(me.peek(), Tok::Ident(_)))
                && (me.string(true)? || (me.committed(p0) && me.expected("\"\\\"\"")?)))
            || { me.back(p0); false }
        });
        me.close(open, hit);
        Ok(hit)
    }

    fn _ident_expr(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat_if(matches!(me.peek(), Tok::Ident(_)))
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Dot) && matches!(me.ahead(1), Tok::Ident(_)) && matches!(me.ahead(2), Tok::Question) && matches!(me.ahead(3), Tok::LParen))
                    && (me._folded()? || (me.committed(p0) && me.expected("Dot")?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true })
                && ({ let _ = ({
                let p0 = me.save();
                ((matches!(me.peek(), Tok::Question) && matches!(me.ahead(1), Tok::LParen))
                    && (me._fallible()? || (me.committed(p0) && me.expected("Question")?)))
                || { me.back(p0); false }
            }); true })
                && ({ let _ = ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::Lt)
                    && (me.attempt(|me| Ok(({
                    let p0 = me.save();
                    (me._type_args()?
                        && (matches!(me.peek(), Tok::LParen) || (me.committed(p0) && me.expected("LParen")?)))
                    || { me.back(p0); false }
                }))) || (me.committed(p0) && me.expected("Lt")?)))
                || { me.back(p0); false }
            }); true })
                && ((
                me._args()?
                || ({
                    let p0 = me.save();
                    (!me.flags.nostruct
                        && (me._struct_body()? || (me.committed(p0) && me.expected("LBrace")?)))
                    || { me.back(p0); false }
                })
                || true
            ) || (me.committed(p0) && me.expected("\"\"")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _folded(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::Folded);
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Dot)
                && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?)))
            || { me.back(p0); false }
        });
        me.close(open, hit);
        Ok(hit)
    }

    fn _fallible(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::Fallible);
        let hit = me.eat(&Tok::Question);
        me.close(open, hit);
        Ok(hit)
    }

    fn _type_args(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::TypeArgs);
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Lt)
                && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?))
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Comma)
                    && (me._type()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true })
                && (me.eat(&Tok::Gt) || (me.committed(p0) && me.expected("Gt")?)))
            || { me.back(p0); false }
        });
        me.close(open, hit);
        Ok(hit)
    }

    fn _struct_body(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::LBrace)
                && (({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = ({ loop { let p = me.cur; if matches!(me.peek(), Tok::RBrace) { break; } if !me._s_field()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }); me.flags.nostruct = old; hit }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _s_field(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _lambda_head(&mut self) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                (me.eat(&Tok::LParen)
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                    && (me.eat(&Tok::Arrow) || (me.committed(p0) && me.expected("Arrow")?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (me.eat(&Tok::LParen)
                    && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?))
                    && ({ loop { let p = me.cur; if !({
                    let p0 = me.save();
                    (me.eat(&Tok::Comma)
                        && (me.eat_if(matches!(me.peek(), Tok::Ident(_))) || (me.committed(p0) && me.expected("\"_\"")?)))
                    || { me.back(p0); false }
                }) || me.cur == p { break; } } true })
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?))
                    && (me.eat(&Tok::Arrow) || (me.committed(p0) && me.expected("Arrow")?)))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _lambda(&mut self) -> R {
        let me = self;
        let open = me.open(Kind::Lambda);
        let hit = ({
            let p0 = me.save();
            ((
                me.eat_if(matches!(me.peek(), Tok::Ident(_)))
                || ({
                    let p0 = me.save();
                    (me.eat(&Tok::LParen)
                        && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._ident()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                        && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
                    || { me.back(p0); false }
                })
            )
                && (me.eat(&Tok::Arrow) || (me.committed(p0) && me.expected("Arrow")?))
                && ((
                me._block()?
                || ({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = me._expr()?; me.flags.nostruct = old; hit })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        me.close(open, hit);
        Ok(hit)
    }

    fn _array_lit(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::LBracket)
                && (({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = (
                ({
                    let p0 = me.save();
                    (me.eat(&Tok::Colon)
                        && (me.eat(&Tok::RBracket) || (me.committed(p0) && me.expected("RBracket")?)))
                    || { me.back(p0); false }
                })
                || me.eat(&Tok::RBracket)
                || ({
                    let p0 = me.save();
                    (me._expr()?
                        && ((
                        ({
                            let p0 = me.save();
                            (me.eat(&Tok::Colon)
                                && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                                && ({ loop { let p = me.cur; if !({
                                let p0 = me.save();
                                (me.eat(&Tok::Comma)
                                    && ((
                                    matches!(me.peek(), Tok::RBracket)
                                    || ({
                                        let p0 = me.save();
                                        (me._expr()?
                                            && (me.eat(&Tok::Colon) || (me.committed(p0) && me.expected("Colon")?))
                                            && (me._expr()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                                        || { me.back(p0); false }
                                    })
                                ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                                || { me.back(p0); false }
                            }) || me.cur == p { break; } } true })
                                && (me.eat(&Tok::RBracket) || (me.committed(p0) && me.expected("RBracket")?)))
                            || { me.back(p0); false }
                        })
                        || ({
                            let p0 = me.save();
                            (({ loop { let p = me.cur; if !({
                                let p0 = me.save();
                                (me.eat(&Tok::Comma)
                                    && ((
                                    matches!(me.peek(), Tok::RBracket)
                                    || me._expr()?
                                ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
                                || { me.back(p0); false }
                            }) || me.cur == p { break; } } true })
                                && (me.eat(&Tok::RBracket) || (me.committed(p0) && me.expected("RBracket")?)))
                            || { me.back(p0); false }
                        })
                    ) || (me.committed(p0) && me.expected("RBracket")?)))
                    || { me.back(p0); false }
                })
            ); me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _match(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me.eat(&Tok::Match)
                && (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit }) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && (me.eat(&Tok::LBrace) || (me.committed(p0) && me.expected("LBrace")?))
                && (({ loop { if (matches!(me.peek(), Tok::RBrace) || matches!(me.peek(), Tok::Eof)) { break; } let p = me.cur; if !me._arm()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _arm(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._pattern()?
                && (me.eat(&Tok::FatArrow) || (me.committed(p0) && me.expected("FatArrow")?))
                && ((
                ({
                    let p0 = me.save();
                    (me._block()?
                        && ({ let _ = me.eat(&Tok::Comma); true }))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (me._expr()?
                        && ((
                        me.eat(&Tok::Comma)
                        || matches!(me.peek(), Tok::RBrace)
                    ) || (me.committed(p0) && me.expected("RBrace")?)))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _pattern(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (me._ident()?
                && ({ loop { let p = me.cur; if !({
                let p0 = me.save();
                (me.eat(&Tok::Dot)
                    && (me._ident()? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?)))
                || { me.back(p0); false }
            }) || me.cur == p { break; } } true })
                && ({ let _ = ({
                let p0 = me.save();
                (me.eat(&Tok::LParen)
                    && (({ loop { let p = me.cur; if matches!(me.peek(), Tok::RParen) { break; } if !me._ident()? { me.refuse_past((me.here().0, me.here().1), |me| rule!(ExpectedIdent, found = me.found()))?; } if !me.eat(&Tok::Comma) || me.cur == p { break; } } true }) || (me.committed(p0) && me.expected("\"\"")?))
                    && (me.eat(&Tok::RParen) || (me.committed(p0) && me.expected("RParen")?)))
                || { me.back(p0); false }
            }); true }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _if_expr(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ let from = me.cur; me.eat(&Tok::If) && ({ let l_i = Capture { from, to: me.cur };
                (me._if_tail(l_i)? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }

    fn _if_tail(&mut self, l_i: Capture) -> R {
        let me = self;
        let hit = ({
                let p0 = me.save();
                (matches!(me.peek(), Tok::Let)
                    && (me.refuse((me.line_of(l_i), me.here().1), |me| rule!(IfLetExpr))? || (me.committed(p0) && me.refuse((me.line_of(l_i), me.here().1), |me| rule!(IfLetExpr))?)))
                || { me.back(p0); false }
            })
            || ({
                let p0 = me.save();
                (({ let old = std::mem::replace(&mut me.flags.nostruct, true); let hit = me._expr()?; me.flags.nostruct = old; hit })
                    && (me._if_branch()? || (me.committed(p0) && me.expected("LBrace")?))
                    && ({ let _ = ({
                    let p0 = me.save();
                    (me.eat(&Tok::Else)
                        && ((
                        ({
                            let p0 = me.save();
                            (({ let from = me.cur; me.eat(&Tok::If) && ({ let l_i = Capture { from, to: me.cur };
                                (me._if_tail(l_i)? || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?)) }) }))
                            || { me.back(p0); false }
                        })
                        || me._if_branch()?
                    ) || (me.committed(p0) && me.expected("LBrace")?)))
                    || { me.back(p0); false }
                }); true }))
                || { me.back(p0); false }
            });
        Ok(hit)
    }

    fn _if_branch(&mut self) -> R {
        let me = self;
        let hit = ({
            let p0 = me.save();
            (({ let from = me.cur; me.eat(&Tok::LBrace) && ({ let l_b = Capture { from, to: me.cur };
                ((
                ({
                    let p0 = me.save();
                    ((matches!(me.peek(), Tok::Let) || matches!(me.peek(), Tok::Return) || matches!(me.peek(), Tok::While) || matches!(me.peek(), Tok::For) || matches!(me.peek(), Tok::Region) || matches!(me.peek(), Tok::Drop) || matches!(me.peek(), Tok::Break) || matches!(me.peek(), Tok::Continue))
                        && (me.refuse((me.here().0, me.here().1), |me| rule!(IfExprStatements))? || (me.committed(p0) && me.refuse((me.here().0, me.here().1), |me| rule!(IfExprStatements))?)))
                    || { me.back(p0); false }
                })
                || ({
                    let p0 = me.save();
                    (({ let old = std::mem::replace(&mut me.flags.nostruct, false); let hit = me._expr()?; me.flags.nostruct = old; hit })
                        && ((
                        matches!(me.peek(), Tok::RBrace)
                        || me.refuse((me.line_of(l_b), me.here().1), |me| rule!(IfExprStatements))?
                    ) || (me.committed(p0) && me.refuse((me.line_of(l_b), me.here().1), |me| rule!(IfExprStatements))?)))
                    || { me.back(p0); false }
                })
            ) || (me.committed(p0) && me.refuse_past((me.here().0, me.here().1), |me| rule!(UnexpectedToken, found = me.found()))?))
                && (me.eat(&Tok::RBrace) || (me.committed(p0) && me.expected("RBrace")?)) }) }))
            || { me.back(p0); false }
        });
        Ok(hit)
    }
}
