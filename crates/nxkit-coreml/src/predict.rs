//! `Model::predict_async`.

use std::cell::Cell;

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_core_ml::{
    MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLPredictionOptions,
};
use objc2_foundation::{NSDictionary, NSError, NSString};

use crate::error::{CoreMlError, Result, ns_text};
use crate::model::{Model, TensorInfo};
use crate::tensor::SharedTensor;

fn check(
    what: &str,
    given: &[(&str, &SharedTensor)],
    wanted: &[TensorInfo],
    all_needed: bool,
) -> Result<()> {
    for (name, t) in given {
        let Some(info) = wanted.iter().find(|i| i.name == *name) else {
            return Err(CoreMlError::Invalid(format!(
                "the model has no {what} {name}"
            )));
        };
        if !info.shape.is_empty() && info.shape != t.shape() {
            return Err(CoreMlError::Invalid(format!(
                "{what} {name} has shape {:?}, the model wants {:?}",
                t.shape(),
                info.shape
            )));
        }
    }
    if all_needed {
        for info in wanted {
            if !given.iter().any(|(n, _)| *n == info.name) {
                return Err(CoreMlError::Invalid(format!(
                    "{what} {} is missing",
                    info.name
                )));
            }
        }
    }
    Ok(())
}

impl Model {
    /// Starts one prediction and returns at once. `inputs` and `outputs` name the tensors by the feature names of the
    /// model. Every input must be given. The outputs are set as `outputBackings`, so Core ML writes the results into
    /// them. An output that is left out is not backed: Core ML makes its own array for it, which this call does not
    /// return, so a caller names every output it wants.
    ///
    /// `done` runs exactly once, on a Core ML thread, when the outputs are written or the prediction failed. It also
    /// runs (on the calling thread, before this returns) when the arguments do not fit the model. The tensors stay
    /// alive until then. The caller must not touch the output tensors, and must not write the input tensors, before
    /// `done` runs. The caller signals an `MTLSharedEvent` from `done` for the GPU to wait on.
    pub fn predict_async(
        &self,
        inputs: &[(&str, &SharedTensor)],
        outputs: &[(&str, &SharedTensor)],
        done: impl FnOnce(Result<()>) + Send + 'static,
    ) {
        let checked = check("input", inputs, self.inputs(), true)
            .and_then(|()| check("output", outputs, self.outputs(), false));
        if let Err(e) = checked {
            done(Err(e));
            return;
        }
        // The tensors are shared by `Arc`: the block holds a clone each, so the pages outlive the prediction.
        let keep: Vec<SharedTensor> = inputs
            .iter()
            .chain(outputs.iter())
            .map(|(_, t)| (*t).clone())
            .collect();
        autoreleasepool(|_| {
            // SAFETY: the objects are made here and only passed to Core ML. The dictionary values are
            // `MLFeatureValue`s and `MLMultiArray`s, as the two APIs want.
            let (provider, options) = unsafe {
                let keys: Vec<Retained<NSString>> =
                    inputs.iter().map(|(n, _)| NSString::from_str(n)).collect();
                let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
                let values: Vec<Retained<AnyObject>> = inputs
                    .iter()
                    .map(|(_, t)| {
                        Retained::into_super(Retained::into_super(
                            MLFeatureValue::featureValueWithMultiArray(t.multi_array()),
                        ))
                    })
                    .collect();
                let dict: Retained<NSDictionary<NSString, AnyObject>> =
                    NSDictionary::from_retained_objects(&key_refs, &values);
                let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                    MLDictionaryFeatureProvider::alloc(),
                    &dict,
                );
                let options = MLPredictionOptions::new();
                let okeys: Vec<Retained<NSString>> =
                    outputs.iter().map(|(n, _)| NSString::from_str(n)).collect();
                let okey_refs: Vec<&NSString> = okeys.iter().map(|k| &**k).collect();
                let ovalues: Vec<Retained<AnyObject>> = outputs
                    .iter()
                    .map(|(_, t)| {
                        Retained::into_super(Retained::into_super(t.multi_array().clone()))
                    })
                    .collect();
                options
                    .setOutputBackings(&NSDictionary::from_retained_objects(&okey_refs, &ovalues));
                (provider, options)
            };
            let provider = match provider {
                Ok(p) => p,
                Err(e) => {
                    done(Err(CoreMlError::Predict(ns_text(&e))));
                    return;
                }
            };
            let done_cell = Cell::new(Some(done));
            let handler = RcBlock::new(
                move |out: *mut ProtocolObject<dyn MLFeatureProvider>, err: *mut NSError| {
                    let _keep = &keep;
                    let result = if out.is_null() {
                        // SAFETY: Core ML passes an error when there is no output, valid for the call.
                        let text = unsafe { Retained::retain(err) }
                            .map_or_else(|| "no output and no error".to_string(), |e| ns_text(&e));
                        Err(CoreMlError::Predict(text))
                    } else {
                        Ok(())
                    };
                    if let Some(done) = done_cell.take() {
                        done(result);
                    }
                },
            );
            let provider: &ProtocolObject<dyn MLFeatureProvider> =
                ProtocolObject::from_ref(&*provider);
            // SAFETY: provider, options and block are valid. Core ML copies the block and retains the others.
            unsafe {
                self.model
                    .predictionFromFeatures_options_completionHandler(provider, &options, &handler);
            }
        });
    }
}
