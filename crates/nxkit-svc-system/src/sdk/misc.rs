//! The error applet, server name resolution and parental controls take-overs. Source: The engine `dm/host/hle_misc.c`
//! (the err, nsd and pctl sections).
//!
//! `err::ShowError` opens the error applet for the time of the call and logs the module and description numbers. A
//! host does not resolve server names, so `nsd::Resolve` fails. Parental controls answer as a console without
//! restriction that never allows communication with others: free communication is not available, the 3D (VR) mode is
//! not permitted and the restriction can be configured at once. `CheckStereoVisionPermission` answering "not permitted"
//! keeps a game in 2D, because the host has no VR display.

use super::oe_state::AppletGuard;
use super::{fail, log, nothing, ok, zero};

/// `err::ShowError(Result)`.
extern "C" fn show_error(result: u32) {
    let _applet = AppletGuard::new("error");
    log(&format!(
        "err::ShowError Result={result:#x} (module {}, description {})",
        result & 0x1ff,
        (result >> 9) & 0x1fff
    ));
}

table! {
    "_ZN2nn3err9ShowErrorENS_6ResultE" => show_error,
    "_ZN2nn3nsd7ResolveEPNS0_4FqdnERKS1_" => fail,
    "_ZN2nn3nsd9ResolveExEPNS0_4FqdnERKS1_" => fail,
    "_ZN2nn4pctl32CheckFreeCommunicationPermissionEb" => zero,
    "_ZN2nn4pctl25TryBeginFreeCommunicationEb" => zero,
    "_ZN2nn4pctl26GetStereoVisionRestrictionEv" => zero,
    "_ZN2nn4pctl26SetStereoVisionRestrictionEb" => nothing,
    "_ZN2nn4pctl42ConfirmStereoVisionRestrictionConfigurableEv" => ok,
    "_ZN2nn4pctl53RequestStereoVisionRestrictionConfigurationPermissionEv" => ok,
    "_ZN2nn4pctl20EndFreeCommunicationEv" => nothing,
    "_ZN2nn4pctl28IsFreeCommunicationAvailableEv" => zero,
    "_ZN2nn4pctl33CheckAgcPermissionUnderMinimumAgeERKNS_7account3UidE" => zero,
    "_ZN2nn4pctl27CheckStereoVisionPermissionEb" => zero,
}
