//! Manrope, the face of NX2Apple's interface, for the host's own screens (`homeviews.rs`).
//!
//! The variable font is in the binary (`assets/Manrope.ttf`, SIL Open Font License 1.1, `assets/Manrope-OFL.txt`) and
//! is read with CoreText, as NX2Apple's `Typography.swift` reads it: one descriptor, and the weight on the `wght` axis.
//! Where it cannot be read, the system's face of the same weight is used.

use std::cell::OnceCell;
use std::ffi::c_void;

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSFont, NSFontDescriptor, NSFontFeatureSelectorIdentifierKey, NSFontFeatureSettingsAttribute,
    NSFontFeatureTypeIdentifierKey, NSFontVariationAttribute, NSFontWeight, NSFontWeightBold,
    NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSNumber, NSString};

static MANROPE: &[u8] = include_bytes!("../../assets/Manrope.ttf");

/// The tag of the weight axis, `wght`.
const WGHT: u32 = 0x7767_6874;

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    /// Returns a +1 `CTFontDescriptorRef` for the font in `data` (a `CFDataRef`), or null.
    fn CTFontManagerCreateFontDescriptorFromData(data: *const c_void) -> *mut c_void;
}

/// A weight of the interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Weight {
    /// 400.
    Regular,
    /// 500: the body of the console mode (`console.typography.body`).
    Medium,
    /// 600.
    Semibold,
    /// 700.
    Bold,
}

impl Weight {
    fn axis(self) -> f64 {
        match self {
            Weight::Regular => 400.0,
            Weight::Medium => 500.0,
            Weight::Semibold => 600.0,
            Weight::Bold => 700.0,
        }
    }

    fn system(self) -> NSFontWeight {
        // SAFETY: the weights are constants of AppKit.
        unsafe {
            match self {
                Weight::Regular => NSFontWeightRegular,
                Weight::Medium => NSFontWeightMedium,
                Weight::Semibold => NSFontWeightSemibold,
                Weight::Bold => NSFontWeightBold,
            }
        }
    }
}

thread_local! {
    // Per thread, not process-wide in fact (W26): the views are built on the main thread, which reads the face once.
    static FACE: OnceCell<Option<Retained<NSFontDescriptor>>> = const { OnceCell::new() };
}

fn face() -> Option<Retained<NSFontDescriptor>> {
    FACE.with(|f| {
        f.get_or_init(|| {
            let data = NSData::with_bytes(MANROPE);
            let ptr: *const NSData = &*data;
            // SAFETY: `NSData` is toll-free bridged to `CFData`. The function follows the Create rule: the returned
            // descriptor (toll-free bridged to `NSFontDescriptor`) is owned by the caller.
            unsafe {
                let raw = CTFontManagerCreateFontDescriptorFromData(ptr.cast());
                Retained::from_raw(raw.cast::<NSFontDescriptor>())
            }
        })
        .clone()
    })
}

/// Manrope at `size` points and `weight`, else the system's face.
pub(super) fn manrope(size: f64, weight: Weight) -> Retained<NSFont> {
    let made = face().and_then(|base| {
        let tag = NSNumber::new_u32(WGHT);
        let value = NSNumber::new_f64(weight.axis());
        let axes: Retained<NSDictionary<NSNumber, NSNumber>> =
            NSDictionary::from_slices(&[&*tag], &[&*value]);
        // SAFETY: `NSFontVariationAttribute` is a constant of AppKit. The attributes are a dictionary of an
        // `NSString` key and a dictionary of axis tags to values, as the attribute documents.
        unsafe {
            let key: &NSString = NSFontVariationAttribute;
            let axes: &AnyObject = &axes;
            let attrs = NSDictionary::from_slices(&[key], &[axes]);
            let desc = base.fontDescriptorByAddingAttributes(&attrs);
            NSFont::fontWithDescriptor_size(&desc, size)
        }
    });
    made.unwrap_or_else(|| NSFont::systemFontOfSize_weight(size, weight.system()))
}

/// `font` with digits of one width (the number spacing feature 6, monospaced numbers 0), for counts that change while
/// they are read.
pub(super) fn tabular(font: &NSFont) -> Retained<NSFont> {
    let setting: Retained<NSDictionary<NSString, NSNumber>> = {
        // SAFETY: the keys are constants of AppKit.
        let (kind, selector) = unsafe { (NSFontFeatureTypeIdentifierKey, NSFontFeatureSelectorIdentifierKey) };
        NSDictionary::from_slices(&[kind, selector], &[&*NSNumber::new_u32(6), &*NSNumber::new_u32(0)])
    };
    let settings = NSArray::from_slice(&[&*setting]);
    // SAFETY: `NSFontFeatureSettingsAttribute` is a constant of AppKit. Its value is an array of dictionaries of the
    // feature type and selector, as the attribute documents.
    let desc = unsafe {
        let key: &NSString = NSFontFeatureSettingsAttribute;
        let value: &AnyObject = &settings;
        let attrs = NSDictionary::from_slices(&[key], &[value]);
        font.fontDescriptor().fontDescriptorByAddingAttributes(&attrs)
    };
    NSFont::fontWithDescriptor_size(&desc, font.pointSize()).unwrap_or_else(|| font.retain())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manrope_is_read_from_the_binary() {
        assert!(face().is_some(), "the descriptor of Manrope");
        let bold = manrope(20.0, Weight::Bold);
        assert!(
            bold.familyName()
                .is_some_and(|n| n.to_string().contains("Manrope"))
        );
        assert_eq!(bold.pointSize(), 20.0);
    }
}
