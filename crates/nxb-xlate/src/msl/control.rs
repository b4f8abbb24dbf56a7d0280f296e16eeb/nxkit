//! Functions, blocks and statements of the structured program.

use super::emit::{Em, R};
use super::types::{literal, msl_type};
use crate::ir::OperandType;
use crate::structured::ast::{A, BlockType, Kind};
use crate::structured::types::AggType;
use std::collections::BTreeSet;

impl Em<'_> {
    /// The signature parameters of a helper function: its arguments and the shared parameters.
    pub fn signature(&self, i: usize) -> R<String> {
        let f = self.info.functions.get(i).ok_or("unknown function")?;
        let mut ps = Vec::new();
        for (n, t) in f
            .in_arguments
            .iter()
            .chain(f.out_arguments.iter())
            .enumerate()
        {
            ps.push(format!("thread {}& a{n}", msl_type(*t)?));
        }
        let helper = super::hmma::is_helper(self, i);
        if helper {
            ps.push(super::hmma::PARAMS.to_string());
        }
        ps.extend(self.shared.params.iter().map(|(d, _)| d.clone()));
        Ok(format!(
            "{}{} fn{i}({})",
            if helper {
                "inline __attribute__((always_inline)) "
            } else {
                ""
            },
            msl_type(f.return_type)?,
            ps.join(", ")
        ))
    }

    /// The body of function `i`: its locals and statements. The entry adds the trailing return of the main function.
    pub fn body(&mut self, i: usize) -> R<String> {
        if let Some(b) = super::hmma::body(self, i)? {
            return Ok(b);
        }
        if let Some(b) = super::shared_store::body(self, i)? {
            return Ok(b);
        }
        self.func = i;
        self.may_return = false;
        self.hmma_caller = false;
        let f = self.info.functions.get(i).ok_or("unknown function")?;
        let main = f.main_block;
        let locals = f.locals.clone();
        let ret = f.return_type;
        let saved = std::mem::take(&mut self.out);
        if i == 0 && self.kill() {
            self.raw(&format!("bool {} = false;", super::facts_ps::KILL));
        }
        let mut decls = Vec::new();
        for l in locals {
            let Kind::Operand { var_type, .. } = self.info.ast.kind(l) else {
                return Err("local is not an operand".into());
            };
            let ty = *var_type;
            decls.push((msl_type(ty)?, l, literal(ty, 0)));
        }
        let bare = if self.opt.plain_decls {
            BTreeSet::new()
        } else {
            super::decls::uninit(self.info, i, &self.shared.skip)
        };
        for d in super::decls::lines(&decls, &bare, self.opt.plain_decls) {
            self.raw(&d);
        }
        self.cur = Some(main);
        self.children(main)?;
        if i != 0 && ret != AggType::VOID {
            self.line(&format!("return {};", literal(ret, 0)));
        }
        let mut out = std::mem::replace(&mut self.out, saved);
        if self.hmma_caller {
            out.insert_str(0, &format!("    {}\n", super::hmma::DECL));
        }
        Ok(out)
    }

    fn children(&mut self, b: A) -> R<()> {
        let list = self.info.ast.children(b);
        let mut k = 0;
        while k < list.len() {
            let n = list[k];
            k += 1;
            if self.shared.skip.contains(&n) {
                continue;
            }
            match self.info.ast.kind(n).clone() {
                Kind::Block(d) => match d.ty {
                    BlockType::If => {
                        let has_else = list
                            .get(k)
                            .is_some_and(|e| self.info.ast.block_type(*e) == Some(BlockType::Else));
                        self.if_block(n, d.cond, has_else.then(|| list[k]))?;
                        k += usize::from(has_else);
                    }
                    BlockType::DoWhile => self.loop_block(n, d.cond)?,
                    other => return Err(format!("unported: block type {other:?}")),
                },
                Kind::Assign { dest, src } => self.assign(dest, src)?,
                Kind::Operation(o) => {
                    self.cur = Some(b);
                    self.operation(&o)?;
                    self.flush();
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn scope(&mut self, b: A) -> R<()> {
        let saved = self.cur;
        self.cur = Some(b);
        self.ind += 1;
        let r = self.children(b);
        self.ind -= 1;
        self.cur = saved;
        r
    }

    fn if_block(&mut self, b: A, cond: Option<A>, els: Option<A>) -> R<()> {
        let c = cond.ok_or("if without condition")?;
        let v = self.get(AggType::BOOL, c)?;
        self.line(&format!("if ({})", v.s));
        self.raw("{");
        self.scope(b)?;
        self.raw("}");
        if let Some(e) = els {
            self.raw("else");
            self.raw("{");
            self.scope(e)?;
            self.raw("}");
        }
        Ok(())
    }

    fn loop_block(&mut self, b: A, cond: Option<A>) -> R<()> {
        let c = cond.ok_or("loop without condition")?;
        self.cur = Some(b);
        let (tmp, notes) = (self.tmp, self.notes.len());
        self.get(AggType::BOOL, c)?;
        let statements = !std::mem::take(&mut self.pre).is_empty();
        self.tmp = tmp;
        self.notes.truncate(notes);
        if statements {
            return self.statement_loop(b, c);
        }
        self.line("do");
        self.raw("{");
        self.loops.push(None);
        let r = self.scope(b);
        self.loops.pop();
        r?;
        self.cur = Some(b);
        let v = self.get(AggType::BOOL, c)?;
        self.raw(&format!("}} while ({});", v.s));
        Ok(())
    }

    /// A loop whose condition needs statements. They cannot sit in the `while` of a `do` loop, so the body runs once
    /// in an inner loop that a `continue` leaves, a `break` sets the flag and leaves, and the condition follows it.
    fn statement_loop(&mut self, b: A, c: A) -> R<()> {
        let flag = format!("lb{}", self.tmp);
        self.tmp += 1;
        self.line("while (true)");
        self.raw("{");
        self.ind += 1;
        self.raw(&format!("bool {flag} = false;"));
        self.raw("do");
        self.raw("{");
        self.loops.push(Some(flag.clone()));
        let r = self.scope(b);
        self.loops.pop();
        r?;
        self.raw("} while (false);");
        self.raw(&format!("if ({flag})"));
        self.raw("{");
        self.raw("    break;");
        self.raw("}");
        self.cur = Some(b);
        let v = self.get(AggType::BOOL, c)?;
        self.line(&format!("if (!({}))", v.s));
        self.raw("{");
        self.raw("    break;");
        self.raw("}");
        self.ind -= 1;
        self.raw("}");
        Ok(())
    }

    fn assign(&mut self, dest: A, src: A) -> R<()> {
        let Kind::Operand {
            ty,
            var_type,
            value,
        } = self.info.ast.kind(dest).clone()
        else {
            return Err("assignment destination is not an operand".into());
        };
        let name = match ty {
            OperandType::LocalVariable => format!("v{}", dest.0),
            OperandType::Argument => format!("a{value}"),
            other => return Err(format!("invalid assignment destination {other:?}")),
        };
        let v = self.get(var_type, src)?;
        self.line(&format!("{name} = {};", v.s));
        Ok(())
    }
}
