//! RemoteIO output for iOS. The upstream mixer and guest services remain unchanged.
//! This is not AUHAL and never queries a desktop AudioObject/device.
#![allow(unsafe_code)]

/// The mixer's realtime callback: interleaved stereo 48 kHz float32.
#[cfg(target_os = "ios")]
pub type Fill = Box<dyn FnMut(&mut [f32]) + Send>;

/// Foreground intent and the system interruption gate are independent: a focus
/// notification must never override an interruption still in progress.
#[derive(Clone, Copy)]
#[cfg(any(target_os = "ios", test))]
struct PlaybackIntent {
    active: bool,
    interrupted: bool,
    resume_allowed: bool,
}
#[cfg(any(target_os = "ios", test))]
impl PlaybackIntent {
    const fn should_play(self) -> bool {
        self.active && !self.interrupted && self.resume_allowed
    }

    fn set_active(&mut self, active: bool) {
        self.active = active;
        // A new foreground/play request can resume after an interruption ended,
        // but it must not clear an interruption that is still in progress.
        if active {
            self.resume_allowed = true;
        }
    }

    fn session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::InterruptionBegan => self.interrupted = true,
            SessionEvent::InterruptionEnded { should_resume } => {
                self.interrupted = false;
                // Ending an interruption always clears the system gate. Lack
                // of ShouldResume instead waits for fresh playback intent.
                self.resume_allowed &= should_resume;
            }
            SessionEvent::RouteChanged => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "ios", test))]
enum SessionEvent {
    InterruptionBegan,
    InterruptionEnded { should_resume: bool },
    RouteChanged,
}
#[cfg(any(target_os = "ios", test))]
impl SessionEvent {
    fn interruption(kind: Option<usize>, options: Option<usize>) -> Option<Self> {
        // AVAudioSessionInterruptionType: Ended = 0, Began = 1.
        // AVAudioSessionInterruptionOptions: ShouldResume = 1 << 0.
        match kind {
            Some(1) => Some(Self::InterruptionBegan),
            Some(0) => Some(Self::InterruptionEnded {
                should_resume: options.is_some_and(|options| options & 1 != 0),
            }),
            _ => None,
        }
    }

    fn route_change(reason: Option<usize>) -> Option<Self> {
        // Ignore our own category changes (3) and unknown (0).
        reason
            .filter(|&reason| reason != 0 && reason != 3)
            .map(|_| Self::RouteChanged)
    }
}

#[cfg(any(target_os = "ios", test))]
fn sample_count(frames: u32, buffers: u32, channels: u32, bytes: u32) -> Option<usize> {
    if buffers != 1 || channels != 2 || bytes % 4 != 0 {
        return None;
    }
    let samples = usize::try_from(frames).ok()?.checked_mul(2)?;
    (samples <= bytes as usize / 4).then_some(samples)
}

#[cfg(target_os = "ios")]
mod device {
    use super::{Fill, PlaybackIntent, SessionEvent, sample_count};
    use objc2_audio_toolbox::{
        AudioComponentDescription, AudioComponentFindNext, AudioComponentInstance,
        AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioOutputUnitStart,
        AudioOutputUnitStop, AudioUnitInitialize, AudioUnitRenderActionFlags, AudioUnitSetProperty,
        AudioUnitUninitialize,
    };
    use objc2_avf_audio::{AVAudioSession, AVAudioSessionCategoryPlayback};
    use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Mutex;

    // Keep the established notification API supported by the iOS deployment
    // floor rather than link replacement notifications from a newer SDK.
    #[allow(deprecated)]
    mod notifications {
        use super::SessionEvent;
        use block2::RcBlock;
        use objc2::{msg_send, rc::Retained};
        use objc2_avf_audio::{
            AVAudioSession, AVAudioSessionInterruptionNotification,
            AVAudioSessionInterruptionOptionKey, AVAudioSessionInterruptionTypeKey,
            AVAudioSessionRouteChangeNotification, AVAudioSessionRouteChangeReasonKey,
        };
        use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue, NSString};
        use std::{ptr::NonNull, sync::Once};

        fn number(notification: &NSNotification, key: &NSString) -> Option<usize> {
            let info = notification.userInfo()?;
            let value = info.objectForKey(key)?;
            // SAFETY: Apple's session notification contract specifies NSNumber values.
            Some(unsafe { msg_send![&*value, unsignedIntegerValue] })
        }

        fn enqueue(event: SessionEvent) {
            // Always enqueue, including when the notification arrived on main.
            // Never reenter the control lock during setCategory/setActive.
            NSOperationQueue::mainQueue().addOperationWithBlock(&RcBlock::new(move || {
                if let Err(error) = super::session_event(event) {
                    eprintln!("nx2ipa audio: {error}");
                }
            }));
        }

        pub fn install() {
            static INSTALLED: Once = Once::new();
            // SAFETY: session notification constants and retained observers stay valid for the process.
            INSTALLED.call_once(|| unsafe {
                let center = NSNotificationCenter::defaultCenter();
                let session = AVAudioSession::sharedInstance();
                let interruption = center.addObserverForName_object_queue_usingBlock(
                    Some(AVAudioSessionInterruptionNotification),
                    Some(&session),
                    None,
                    &RcBlock::new(|notification: NonNull<NSNotification>| {
                        let notification = notification.as_ref();
                        if let Some(event) = SessionEvent::interruption(
                            number(notification, AVAudioSessionInterruptionTypeKey),
                            number(notification, AVAudioSessionInterruptionOptionKey),
                        ) {
                            enqueue(event);
                        }
                    }),
                );
                let route = center.addObserverForName_object_queue_usingBlock(
                    Some(AVAudioSessionRouteChangeNotification),
                    Some(&session),
                    None,
                    &RcBlock::new(|notification: NonNull<NSNotification>| {
                        // Only actual route/configuration changes need a restart.
                        if let Some(event) = SessionEvent::route_change(number(
                            notification.as_ref(),
                            AVAudioSessionRouteChangeReasonKey,
                        )) {
                            enqueue(event);
                        }
                    }),
                );
                // Process-wide observers deliberately outlive every Output. The
                // blocks own no unit or mixer pointer and consult the registry.
                let _ = Retained::into_raw(interruption);
                let _ = Retained::into_raw(route);
            });
        }
    }

    // Every registration, stop/start and disposal holds this lock. Raw handles
    // never escape it except to their owning Output. The render thread never
    // accesses this registry or acquires a lock.
    struct Control {
        intent: PlaybackIntent,
        units: Vec<usize>,
    }
    static CONTROL: Mutex<Control> = Mutex::new(Control {
        intent: PlaybackIntent {
            active: true,
            interrupted: false,
            resume_allowed: true,
        },
        units: Vec::new(),
    });

    fn activate_session() -> Result<(), String> {
        // SAFETY: the shared session and static playback category are valid;
        // this is called only by serialized control work, never by rendering.
        unsafe {
            let session = AVAudioSession::sharedInstance();
            session
                .setCategory_error(AVAudioSessionCategoryPlayback)
                .map_err(|e| e.to_string())?;
            session
                .setPreferredSampleRate_error(48_000.0)
                .map_err(|e| e.to_string())?;
            session.setActive_error(true).map_err(|e| e.to_string())
        }
    }

    fn update(change: impl FnOnce(&mut Control)) -> Result<(), String> {
        let mut control = CONTROL.lock().map_err(|_| "Audio control lock poisoned")?;
        change(&mut control);
        let mut error = None;
        // Stop every unit first, also for route changes. Continue stopping the
        // remaining units after an error rather than leave them running.
        for &unit in &control.units {
            // SAFETY: the control lock keeps registered units live through this call.
            if let Err(e) = checked("stop", unsafe {
                AudioOutputUnitStop(unit as AudioComponentInstance)
            }) {
                error.get_or_insert(e);
            }
        }
        if let Some(e) = error {
            return Err(e);
        }
        if control.intent.should_play() {
            // No device yet: remember intent without taking another app's audio.
            if control.units.is_empty() {
                return Ok(());
            }
            activate_session()?;
            for &unit in &control.units {
                // SAFETY: the control lock keeps registered units live through this call.
                checked("restart", unsafe {
                    AudioOutputUnitStart(unit as AudioComponentInstance)
                })?;
            }
        } else if !control.intent.interrupted {
            // SAFETY: the shared session is live and control changes are serialized.
            unsafe { AVAudioSession::sharedInstance().setActive_error(false) }
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn set_active(active: bool) -> Result<(), String> {
        notifications::install();
        update(|c| c.intent.set_active(active))
    }
    fn session_event(event: SessionEvent) -> Result<(), String> {
        update(|c| c.intent.session_event(event))
    }
    pub fn route_changed() -> Result<(), String> {
        session_event(SessionEvent::RouteChanged)
    }

    struct Callback {
        fill: Fill,
    }
    #[repr(C)]
    struct RenderCallback {
        proc_: unsafe extern "C-unwind" fn(
            NonNull<c_void>,
            NonNull<AudioUnitRenderActionFlags>,
            NonNull<AudioTimeStamp>,
            u32,
            u32,
            *mut AudioBufferList,
        ) -> i32,
        refcon: *mut c_void,
    }

    /// Owns a RemoteIO instance and its callback; no borrowed callback can escape.
    pub struct Output {
        unit: AudioComponentInstance,
        callback: NonNull<Callback>,
    }
    // SAFETY: AudioUnit operations happen through one owner. The callback's Fill
    // is touched only by the serialized RemoteIO render thread until Stop/Dispose.
    unsafe impl Send for Output {}
    // SAFETY: shared access exposes only join_status(), which reads no raw state.
    unsafe impl Sync for Output {}

    fn checked(operation: &str, status: i32) -> Result<(), String> {
        if status == 0 {
            Ok(())
        } else {
            Err(format!("RemoteIO {operation}: OSStatus {status}"))
        }
    }

    unsafe extern "C-unwind" fn render(
        context: NonNull<c_void>,
        _flags: NonNull<AudioUnitRenderActionFlags>,
        _time: NonNull<AudioTimeStamp>,
        _bus: u32,
        frames: u32,
        data: *mut AudioBufferList,
    ) -> i32 {
        let Some(data) = NonNull::new(data) else {
            return -50;
        };
        // SAFETY: AudioUnit supplies the live buffer list for this callback only.
        let list = unsafe { data.as_ref() };
        if list.mNumberBuffers != 1 {
            return -50;
        }
        let buffer = &list.mBuffers[0];
        let Some(samples) = sample_count(
            frames,
            list.mNumberBuffers,
            buffer.mNumberChannels,
            buffer.mDataByteSize,
        ) else {
            return -50;
        };
        if samples == 0 {
            return 0;
        }
        if buffer.mData.is_null() || (buffer.mData as usize) % align_of::<f32>() != 0 {
            return -50;
        }
        // SAFETY: checked size/alignment is inside the AudioUnit-owned output buffer.
        let output = unsafe { std::slice::from_raw_parts_mut(buffer.mData.cast::<f32>(), samples) };
        // SAFETY: the callback Box remains alive until this unit has been disposed,
        // and RemoteIO does not invoke two renders for one unit simultaneously.
        let state = unsafe { &mut *context.as_ptr().cast::<Callback>() };
        (state.fill)(output);
        0
    }

    impl Output {
        /// Configure playback without microphone permission, then start RemoteIO.
        pub fn start(fill: Fill) -> Result<Self, String> {
            notifications::install();
            let description = AudioComponentDescription {
                componentType: u32::from_be_bytes(*b"auou"),
                componentSubType: u32::from_be_bytes(*b"rioc"),
                componentManufacturer: u32::from_be_bytes(*b"appl"),
                componentFlags: 0,
                componentFlagsMask: 0,
            };
            // SAFETY: null starts the search; description lives for the call.
            let component = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &description) };
            if component.is_null() {
                return Err("No RemoteIO output component".into());
            }
            let mut unit: AudioComponentInstance = std::ptr::null_mut();
            // SAFETY: component is valid and unit receives the owned instance.
            checked("create", unsafe {
                AudioComponentInstanceNew(component, &mut unit)
            })?;
            if unit.is_null() {
                return Err("RemoteIO returned a null unit".into());
            }
            let callback = NonNull::from(Box::leak(Box::new(Callback { fill })));
            let output = Self { unit, callback };
            let format = AudioStreamBasicDescription {
                mSampleRate: 48_000.0,
                mFormatID: u32::from_be_bytes(*b"lpcm"),
                mFormatFlags: 1 | 8,
                mBytesPerPacket: 8,
                mFramesPerPacket: 1,
                mBytesPerFrame: 8,
                mChannelsPerFrame: 2,
                mBitsPerChannel: 32,
                mReserved: 0,
            };
            let render = RenderCallback {
                proc_: render,
                refcon: callback.as_ptr().cast(),
            };
            // SAFETY: pointers reference complete property values for each call.
            // Bus 0 input scope is the format supplied by the application to output.
            unsafe {
                checked(
                    "format",
                    AudioUnitSetProperty(
                        unit,
                        8,
                        1,
                        0,
                        (&format as *const AudioStreamBasicDescription).cast(),
                        size_of::<AudioStreamBasicDescription>() as u32,
                    ),
                )?;
                checked(
                    "render callback",
                    AudioUnitSetProperty(
                        unit,
                        23,
                        1,
                        0,
                        (&render as *const RenderCallback).cast(),
                        size_of::<RenderCallback>() as u32,
                    ),
                )?;
                checked("initialize", AudioUnitInitialize(unit))?;
            }
            // Publish only fully initialized units, while serializing against
            // focus/interruption changes. Drop the guard before propagating an
            // error because Output::drop acquires the same control lock.
            let started = (|| {
                let mut control = CONTROL.lock().map_err(|_| "Audio control lock poisoned")?;
                if control.intent.should_play() {
                    activate_session()?;
                    // SAFETY: this initialized unit is owned by output until it is registered.
                    checked("start", unsafe { AudioOutputUnitStart(unit) })?;
                }
                control.units.push(unit as usize);
                Ok::<_, String>(())
            })();
            started?;
            Ok(output)
        }

        /// This adapter does not join an audio workgroup. None never means success.
        pub fn join_status(&self) -> Option<i32> {
            None
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            let mut control = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
            control.units.retain(|&unit| unit != self.unit as usize);
            // SAFETY: this is the uniquely owned unit. Disposal ends all callbacks
            // before the Box is released. If disposal fails, leak instead of UAF.
            unsafe {
                let _ = AudioOutputUnitStop(self.unit);
                let _ = AudioUnitUninitialize(self.unit);
                if AudioComponentInstanceDispose(self.unit) == 0 {
                    drop(Box::from_raw(self.callback.as_ptr()));
                }
            }
        }
    }
}

#[cfg(target_os = "ios")]
pub use device::{Output, route_changed, set_active};

#[cfg(test)]
mod tests {
    use super::{PlaybackIntent, SessionEvent, sample_count};

    fn active_intent() -> PlaybackIntent {
        PlaybackIntent {
            active: true,
            interrupted: false,
            resume_allowed: true,
        }
    }

    #[test]
    fn focus_and_route_cannot_override_interruption() {
        let mut intent = active_intent();
        assert!(intent.should_play());
        intent.session_event(SessionEvent::InterruptionBegan);
        assert!(!intent.should_play());
        intent.set_active(false);
        intent.set_active(true);
        intent.session_event(SessionEvent::RouteChanged);
        assert!(intent.interrupted);
        assert!(!intent.should_play());
        intent.session_event(SessionEvent::InterruptionEnded {
            should_resume: true,
        });
        assert!(!intent.interrupted);
        assert!(intent.should_play());
    }

    #[test]
    fn ending_interruption_cannot_resume_in_background() {
        let mut intent = active_intent();
        intent.session_event(SessionEvent::InterruptionBegan);
        intent.set_active(false);
        intent.session_event(SessionEvent::InterruptionEnded {
            should_resume: true,
        });
        assert!(!intent.interrupted);
        assert!(!intent.should_play());
        intent.set_active(true);
        assert!(intent.should_play());
    }

    #[test]
    fn denied_resume_clears_interruption_but_waits_for_fresh_intent() {
        for active in [false, true] {
            let mut intent = active_intent();
            intent.session_event(SessionEvent::InterruptionBegan);
            intent.set_active(active);
            intent.session_event(SessionEvent::InterruptionEnded {
                should_resume: false,
            });
            assert!(!intent.interrupted);
            assert_eq!(intent.active, active);
            assert!(!intent.should_play());
            intent.session_event(SessionEvent::RouteChanged);
            assert!(!intent.should_play());
            intent.set_active(true);
            assert!(intent.should_play());
        }
    }

    #[test]
    fn later_interruption_cannot_replace_fresh_playback_intent() {
        let mut intent = active_intent();
        intent.session_event(SessionEvent::InterruptionBegan);
        intent.session_event(SessionEvent::InterruptionEnded {
            should_resume: false,
        });
        intent.session_event(SessionEvent::InterruptionBegan);
        intent.session_event(SessionEvent::InterruptionEnded {
            should_resume: true,
        });
        assert!(!intent.interrupted);
        assert!(!intent.should_play());
        intent.set_active(true);
        assert!(intent.should_play());
    }

    #[test]
    fn interruption_notifications_always_deliver_an_end() {
        for (options, should_resume) in [
            (None, false),
            (Some(0), false),
            (Some(2), false),
            (Some(1), true),
            (Some(3), true),
        ] {
            let begin = SessionEvent::interruption(Some(1), options).unwrap();
            assert_eq!(begin, SessionEvent::InterruptionBegan);
            let end = SessionEvent::interruption(Some(0), options).unwrap();
            assert_eq!(end, SessionEvent::InterruptionEnded { should_resume });
            let mut intent = active_intent();
            intent.session_event(begin);
            assert!(!intent.should_play());
            intent.session_event(end);
            assert!(!intent.interrupted);
            assert_eq!(intent.should_play(), should_resume);
        }
        for kind in [None, Some(2), Some(usize::MAX)] {
            assert_eq!(SessionEvent::interruption(kind, Some(1)), None);
        }
    }

    #[test]
    fn route_notifications_ignore_category_and_unknown_changes() {
        for reason in [None, Some(0), Some(3)] {
            assert_eq!(SessionEvent::route_change(reason), None);
        }
        for reason in [1, 2, 4, 6, 7, 8] {
            assert_eq!(
                SessionEvent::route_change(Some(reason)),
                Some(SessionEvent::RouteChanged)
            );
        }
    }

    #[test]
    fn stereo_frame_bounds() {
        assert_eq!(sample_count(128, 1, 2, 1024), Some(256));
        assert_eq!(sample_count(128, 1, 2, 512), None);
        assert_eq!(sample_count(128, 2, 1, 1024), None);
        assert_eq!(sample_count(128, 1, 2, 1023), None);
        assert_eq!(sample_count(u32::MAX, 1, 2, u32::MAX), None);
    }
}
