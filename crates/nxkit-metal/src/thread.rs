//! The QoS class of a host thread that encodes for the GPU.

/// Puts the calling thread in the user-interactive QoS class, so that the system runs it on a performance core. Threads
/// that encode a frame are on the frame's critical path; at the default class the system often put them on the
/// efficiency cores, and the caller waited for the slowest of them.
pub fn set_user_interactive() {
    #[cfg(target_vendor = "apple")]
    {
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
        }
        // SAFETY: the call changes only the class of the calling thread and takes plain values.
        let _ = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
    }
}
