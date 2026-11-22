//! HD Rumble for a Switch Pro Controller: the values of the game become the controller's own rumble reports, so it
//! plays both bands with their frequencies as on the Switch. `encoding` builds the reports, `pacer` decides when one
//! goes out. Both are plain logic. The device and the thread that writes are in `sys::hid_rumble`.

pub(crate) mod encoding;
pub(crate) mod pacer;
