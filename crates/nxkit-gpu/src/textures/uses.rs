//! How a frame uses each texture: the input of the texture store and of the encode view (`crate::snapshot`).

use nxkit_nvn::frame::{Cmd, Frame};
use nxkit_nvn::passes::PassPlan;
use nxkit_nvn::resource::TexId;

use super::Use;
use crate::fastmap::{FastMap, FastSet};

/// How each texture of the frame is used. A texture is memoryless only when every attachment use says so and no
/// command samples, copies, presents or reads it.
pub(crate) fn collect_uses(frame: &Frame, plan: &PassPlan) -> FastMap<TexId, Use> {
    let mut other: FastSet<TexId> = FastSet::default();
    let mut read_back: FastSet<TexId> = FastSet::default();
    for c in &frame.cmds {
        match *c {
            Cmd::Draw { reads, .. } => other.extend(frame.list(reads).iter().copied()),
            Cmd::Dispatch { reads, writes } => {
                other.extend(frame.list(reads).iter().copied());
                other.extend(frame.list(writes).iter().copied());
            }
            Cmd::Copy { src, dst } => other.extend(src.into_iter().chain(dst)),
            Cmd::Downsample { src, dst } => other.extend([src, dst]),
            Cmd::Present(id) => {
                other.insert(id);
            }
            Cmd::CpuRead(id) => {
                other.insert(id);
                read_back.insert(id);
            }
            _ => {}
        }
    }
    let mut uses: FastMap<TexId, Use> = FastMap::default();
    for a in &plan.attachments {
        let u = uses.entry(a.tex).or_insert(Use {
            attachment: true,
            memoryless: true,
            cpu_read: false,
            atomic: false,
        });
        u.memoryless &= a.memoryless && !other.contains(&a.tex);
    }
    for id in other {
        uses.entry(id).or_default();
    }
    for id in read_back {
        uses.entry(id).or_default().cpu_read = true;
    }
    uses
}
