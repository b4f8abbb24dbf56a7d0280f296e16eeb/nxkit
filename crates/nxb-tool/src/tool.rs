//! The `Tool` trait and its process implementation.

use crate::invocation::{Completed, Invocation};
use crate::process::{ProcessRegistry, run_process};
use nxb_error::ToolError;
use std::path::PathBuf;
use std::sync::Arc;

/// Identity of an external tool: what is recorded in TOOL_FP (design-v2 6).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ToolId {
    /// Short name (`metal`, `metallib`, `air-opt`).
    pub name: String,
    /// Resolved absolute path.
    pub path: PathBuf,
    /// Version line or digest that identifies the build of the tool.
    pub version: String,
}

/// An external tool behind one interface. The first implementation of every tool is a process per job
/// ([`ProcessTool`]). Persistent workers and servers are later implementations picked by configuration (design-v2 7.3).
pub trait Tool: Send + Sync {
    /// The tool's identity.
    fn id(&self) -> &ToolId;

    /// Run one job. Any failure (non-zero exit, crash, timeout, cancellation) is a `ToolError`.
    fn run(&self, inv: &Invocation) -> Result<Completed, ToolError>;
}

/// A tool run as one child process per job, in its own process group, registered for cancellation.
#[derive(Clone, Debug)]
pub struct ProcessTool {
    id: ToolId,
    registry: Arc<ProcessRegistry>,
}

impl ProcessTool {
    /// The run's process registry.
    pub fn registry(&self) -> Arc<ProcessRegistry> {
        self.registry.clone()
    }

    /// A process tool sharing the run's registry.
    pub fn new(id: ToolId, registry: Arc<ProcessRegistry>) -> Self {
        ProcessTool { id, registry }
    }

    /// A fresh invocation of this tool's program (empty environment, default limits).
    pub fn invocation(&self) -> Invocation {
        Invocation::new(self.id.path.clone())
    }
}

impl Tool for ProcessTool {
    fn id(&self) -> &ToolId {
        &self.id
    }

    fn run(&self, inv: &Invocation) -> Result<Completed, ToolError> {
        run_process(&self.id.name, inv, &self.registry)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn process_tool_runs() {
        let t = ProcessTool::new(
            ToolId {
                name: "echo".into(),
                path: "/bin/echo".into(),
                version: "test".into(),
            },
            Arc::new(ProcessRegistry::new()),
        );
        let out = t.run(&t.invocation().arg("hi")).unwrap();
        assert_eq!(out.stdout, b"hi\n");
        let dynamic: &dyn Tool = &t;
        assert_eq!(dynamic.id().name, "echo");
    }
}
