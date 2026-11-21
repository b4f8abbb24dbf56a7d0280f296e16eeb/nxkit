//! Subgroup operations (ballot, vote, shuffle, swizzle add), barriers and the statement instructions.

use super::emit::{Em, R, Shader, Val};
use super::types::literal;
use crate::ir::Inst;
use crate::ir::agg::AggType;
use crate::structured::ast::{BlockType, OpData};

impl Em<'_> {
    /// Subgroup operations and barriers.
    pub(super) fn subgroup(&mut self, inst: Inst, o: &OpData) -> R<Option<Val>> {
        match inst {
            Inst::Ballot => self.ballot(o).map(Some),
            Inst::VoteAll | Inst::VoteAllEqual | Inst::VoteAny => {
                let a = self.src(o, 0)?;
                let p = self.get(AggType::BOOL, a)?;
                let s = match inst {
                    Inst::VoteAll => format!("simd_all({})", p.s),
                    Inst::VoteAny => format!("simd_any({})", p.s),
                    _ => {
                        let p = self.once(p)?;
                        format!("(simd_all({0}) || !simd_any({0}))", p.s)
                    }
                };
                Ok(Some(Val::new(s, AggType::BOOL)))
            }
            Inst::Shuffle | Inst::ShuffleDown | Inst::ShuffleUp | Inst::ShuffleXor => {
                let a = self.src(o, 0)?;
                let v = self.get(AggType::FP32, a)?;
                let a = self.src(o, 1)?;
                let i = self.get(AggType::U32, a)?;
                let f = match inst {
                    Inst::Shuffle => "simd_shuffle",
                    Inst::ShuffleDown => "simd_shuffle_down",
                    Inst::ShuffleUp => "simd_shuffle_up",
                    _ => "simd_shuffle_xor",
                };
                let call = format!("{f}({}, ushort({}))", v.s, i.s);
                Ok(Some(self.effect(call, AggType::FP32)?))
            }
            Inst::SwizzleAdd => self.swizzle_add(o).map(Some),
            Inst::Barrier => {
                if !self.barrier_diverges() {
                    let flags = self.tg_flags();
                    self.line(&format!("threadgroup_barrier({flags});"));
                }
                Ok(None)
            }
            Inst::SubgroupBarrier => {
                let flags = self.tg_flags();
                self.line(&format!("simdgroup_barrier({flags});"));
                Ok(None)
            }
            Inst::GroupMemoryBarrier | Inst::MemoryBarrier => {
                // MSL 2.3 has no standalone fence, so a compute shader gets a barrier and other stages get nothing.
                if self.shader == Shader::Compute && !self.barrier_diverges() {
                    let flags = if inst == Inst::MemoryBarrier {
                        "mem_flags::mem_device | mem_flags::mem_threadgroup | mem_flags::mem_texture"
                    } else {
                        self.tg_flags()
                    };
                    self.line(&format!("threadgroup_barrier({flags});"));
                }
                Ok(None)
            }
            _ => Err(format!("unported: instruction {:x}", o.inst.0)),
        }
    }

    /// The memory flags of a threadgroup barrier. The spilled shared memory is device memory.
    fn tg_flags(&self) -> &'static str {
        if self.shared.spill {
            "mem_flags::mem_device | mem_flags::mem_threadgroup"
        } else {
            "mem_flags::mem_threadgroup"
        }
    }

    fn barrier_diverges(&self) -> bool {
        !self.props.host.supports_shader_barrier_divergence
            && (self.block_type() != Some(BlockType::Main) || self.may_return || self.func != 0)
    }

    pub(super) fn block_type(&self) -> Option<BlockType> {
        self.cur.and_then(|c| self.info.ast.block_type(c))
    }

    fn ballot(&mut self, o: &OpData) -> R<Val> {
        let a = self.src(o, 0)?;
        let p = self.get(AggType::BOOL, a)?;
        let s = if self.opt.visibility_counter {
            if o.index == 0 {
                format!("({} ? 1u : 0u)", p.s)
            } else {
                "0u".into()
            }
        } else {
            match o.index {
                0 => format!("uint(ulong(simd_ballot({})))", p.s),
                1 => format!("uint(ulong(simd_ballot({})) >> 32)", p.s),
                _ => "0u".into(),
            }
        };
        Ok(Val::new(s, AggType::U32))
    }

    fn swizzle_add(&mut self, o: &OpData) -> R<Val> {
        let a = self.src(o, 0)?;
        let x = self.get(AggType::FP32, a)?;
        let x = self.once(x)?;
        let a = self.src(o, 1)?;
        let y = self.get(AggType::FP32, a)?;
        let y = self.once(y)?;
        let a = self.src(o, 2)?;
        let m = self.get(AggType::U32, a)?;
        let lane = self.plan.lane();
        let idx = format!("(({} >> (({lane} & 3u) << 1u)) & 3u)", m.s);
        let idx = self.hoist(Val::new(idx, AggType::U32))?;
        let s = format!(
            "({} * float4(1.0, -1.0, 1.0, 0.0)[{idx}] + {} * float4(1.0, 1.0, -1.0, 1.0)[{idx}])",
            x.s,
            y.s,
            idx = idx.s
        );
        Ok(Val::new(s, AggType::FP32))
    }

    /// The instructions that are statements.
    pub(super) fn control_op(&mut self, inst: Inst, o: &OpData) -> R<Option<Val>> {
        match inst {
            Inst::Return => {
                self.may_return = true;
                self.return_stmt(o)?;
            }
            Inst::Discard => {
                self.line("discard_fragment();");
                if self.kill() && self.func != 0 {
                    self.line(&format!("{} = true;", super::facts_ps::KILL));
                }
                self.return_default()?;
            }
            Inst::LoopBreak => match self.loops.last().cloned().flatten() {
                Some(flag) => self.line(&format!("{{ {flag} = true; break; }}")),
                None => self.line("break;"),
            },
            Inst::LoopContinue => self.line("continue;"),
            Inst::FSIBegin | Inst::FSIEnd => {
                // Metal orders the accesses of the resources in raster order group 0, which the argument buffer members carry.
                if self.shader != Shader::Fragment {
                    return Err("unported: fragment shader interlock outside a pixel shader".into());
                }
            }
            Inst::Comment => {}
            _ => return Err("unported: geometry stream output".into()),
        }
        Ok(None)
    }

    fn return_stmt(&mut self, o: &OpData) -> R<()> {
        if let Some(a) = o.sources.first() {
            let ty = self.info.functions[self.func].return_type;
            let v = self.get(ty, *a)?;
            self.line(&format!("return {};", v.s));
        } else {
            self.return_default()?;
        }
        Ok(())
    }

    /// A return without a value of the current function.
    pub(super) fn return_default(&mut self) -> R<()> {
        if self.func == 0 {
            if let (Shader::Vertex, Some(exit)) = (self.shader, self.exit.clone()) {
                for l in &exit {
                    self.line(l);
                }
                return Ok(());
            }
            match self.shader {
                Shader::Compute => self.line("return;"),
                Shader::Fragment if self.plan.out_members.is_empty() => self.line("return;"),
                s => {
                    if s == Shader::Vertex {
                        for l in self.plan.vertex_epilogue(true) {
                            self.line(&l);
                        }
                    }
                    self.line("return out;");
                }
            }
            return Ok(());
        }
        let ret = self.info.functions[self.func].return_type;
        if ret == AggType::VOID {
            self.line("return;");
        } else {
            self.line(&format!("return {};", literal(ret, 0)));
        }
        Ok(())
    }
}
