//! The compute plan: which device runs each operation of the program.

use std::sync::{Mutex, mpsc};

use block2::RcBlock;
use objc2::ClassType;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_core_ml::{
    MLCPUComputeDevice, MLComputeDeviceProtocol, MLComputePlan, MLGPUComputeDevice,
    MLNeuralEngineComputeDevice,
};
use objc2_foundation::NSError;

use crate::error::{CoreMlError, Result, ns_text};
use crate::model::Model;

/// A device that a program operation can run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeDevice {
    /// The Neural Engine.
    NeuralEngine,
    /// The GPU.
    Gpu,
    /// The CPU.
    Cpu,
    /// Core ML gave no usage for the operation, or a device of a kind this crate does not know.
    Unknown,
}

/// The devices of all operations of a model, as Core ML plans them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanReport {
    /// Operations counted, without the `const` operations that hold weights.
    pub total: usize,
    /// How many prefer the Neural Engine.
    pub neural_engine: usize,
    /// How many prefer the GPU.
    pub gpu: usize,
    /// How many prefer the CPU.
    pub cpu: usize,
    /// How many have no known device.
    pub unknown: usize,
    /// The operations that do not prefer the Neural Engine, as `(operator name, device)`, in program order.
    pub off_neural_engine: Vec<(String, ComputeDevice)>,
}

impl PlanReport {
    /// True when the program has operations and every one prefers the Neural Engine. A caller that must not fall back
    /// refuses the model otherwise.
    pub fn all_on_neural_engine(&self) -> bool {
        self.total > 0 && self.neural_engine == self.total
    }

    /// Counts one operation.
    pub(crate) fn add(&mut self, op: &str, device: ComputeDevice) {
        self.total += 1;
        match device {
            ComputeDevice::NeuralEngine => self.neural_engine += 1,
            ComputeDevice::Gpu => self.gpu += 1,
            ComputeDevice::Cpu => self.cpu += 1,
            ComputeDevice::Unknown => self.unknown += 1,
        }
        if device != ComputeDevice::NeuralEngine {
            self.off_neural_engine.push((op.to_string(), device));
        }
    }
}

/// A finished plan on its way from the Core ML thread to the caller.
struct SendPlan(Retained<MLComputePlan>);

// SAFETY: a finished `MLComputePlan` is immutable, and the handler passes it on without using it again.
unsafe impl Send for SendPlan {}

fn kind_of(d: &ProtocolObject<dyn MLComputeDeviceProtocol>) -> ComputeDevice {
    if d.isKindOfClass(MLNeuralEngineComputeDevice::class()) {
        ComputeDevice::NeuralEngine
    } else if d.isKindOfClass(MLGPUComputeDevice::class()) {
        ComputeDevice::Gpu
    } else if d.isKindOfClass(MLCPUComputeDevice::class()) {
        ComputeDevice::Cpu
    } else {
        ComputeDevice::Unknown
    }
}

impl Model {
    /// Asks Core ML where it would run each operation of the program (`MLComputePlan`) and counts them per device.
    /// It reads the compiled model again and may take a moment, so call it once after loading, on a background
    /// thread. It needs an ML Program; another model kind gives an error.
    pub fn compute_plan(&self) -> Result<PlanReport> {
        let (tx, rx) = mpsc::channel::<std::result::Result<SendPlan, String>>();
        let tx = Mutex::new(Some(tx));
        let handler = RcBlock::new(move |plan: *mut MLComputePlan, err: *mut NSError| {
            // SAFETY: Core ML passes either a plan or an error, both valid for the call. `retain` keeps what we use.
            let r = unsafe { Retained::retain(plan) }
                .map(SendPlan)
                .ok_or_else(|| {
                    // SAFETY: as above.
                    unsafe { Retained::retain(err) }
                        .map_or_else(|| "no plan and no error".to_string(), |e| ns_text(&e))
                });
            if let Some(tx) = tx.lock().ok().and_then(|mut g| g.take()) {
                let _ = tx.send(r);
            }
        });
        let url = objc2_foundation::NSURL::fileURLWithPath_isDirectory(
            &objc2_foundation::NSString::from_str(&self.compiled.to_string_lossy()),
            true,
        );
        // SAFETY: the URL is the compiled model, the configuration is the one of the loaded model, the block is
        // sendable and Core ML copies it.
        unsafe {
            MLComputePlan::loadContentsOfURL_configuration_completionHandler(
                &url,
                &self.config,
                &handler,
            );
        }
        let plan = rx
            .recv()
            .map_err(|_| CoreMlError::Plan("Core ML dropped the handler".into()))?
            .map_err(CoreMlError::Plan)?;
        autoreleasepool(|_| report(&plan.0))
    }
}

fn report(plan: &MLComputePlan) -> Result<PlanReport> {
    // SAFETY: getters on immutable objects of a finished plan.
    unsafe {
        let program = plan
            .modelStructure()
            .program()
            .ok_or_else(|| CoreMlError::Plan("the model is not an ML Program".into()))?;
        let mut rep = PlanReport::default();
        for function in program.functions().to_vecs().1 {
            for op in function.block().operations().iter() {
                let name = op.operatorName().to_string();
                if name == "const" {
                    continue;
                }
                let device = plan
                    .computeDeviceUsageForMLProgramOperation(&op)
                    .map_or(ComputeDevice::Unknown, |u| {
                        kind_of(&u.preferredComputeDevice())
                    });
                rep.add(&name, device);
            }
        }
        Ok(rep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_counts_and_refuses_fallbacks() {
        let mut r = PlanReport::default();
        assert!(!r.all_on_neural_engine());
        r.add("conv", ComputeDevice::NeuralEngine);
        r.add("softmax", ComputeDevice::NeuralEngine);
        assert!(r.all_on_neural_engine());
        r.add("gather", ComputeDevice::Cpu);
        assert!(!r.all_on_neural_engine());
        assert_eq!((r.total, r.neural_engine, r.cpu), (3, 2, 1));
        assert_eq!(
            r.off_neural_engine,
            vec![("gather".to_string(), ComputeDevice::Cpu)]
        );
    }
}
