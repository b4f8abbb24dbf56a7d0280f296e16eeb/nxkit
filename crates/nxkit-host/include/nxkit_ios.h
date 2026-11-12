#ifndef NX2IPA_RUNTIME_H
#define NX2IPA_RUNTIME_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// All calls require the UIKit main thread. Exactly one game may start per process.
// layer is a borrowed CAMetalLayer; Rust retains it before start returns.
// Paths and touches are borrowed only during their call. No Rust allocation crosses C.
typedef struct { uint32_t id; float x; float y; } Nx2ipaTouch;
bool nx2ipa_start(void *layer, const char *bundle, const char *home,
                  uint32_t maximum_fps, char *error, size_t error_capacity);
void nx2ipa_set_active(bool active);
void nx2ipa_resize(uint32_t width, uint32_t height);
void nx2ipa_touches(const Nx2ipaTouch *points, size_t count);
// Complete virtual P1 snapshot. Npad bits 0..15: A B X Y L3 R3 L R ZL ZR
// Plus Minus Left Up Right Down. Unknown bits are discarded. Axes are x-right,
// y-up, finite [-1, 1]; out-of-range values clamp and nonfinite values become 0.
// Focus loss/stop releases the pad; background snapshots are ignored.
void nx2ipa_virtual_pad(uint64_t buttons, float lx, float ly, float rx, float ry);
// Physical extended-profile gamepads only. Discovery is cached for up to 100 ms.
bool nx2ipa_has_controller(void);
// Current nxkit mode, used to offer guest touchscreen controls only in handheld.
bool nx2ipa_is_handheld(void);
// Zero means the presenter fills its drawable, not an assumed 16:9 picture.
double nx2ipa_picture_aspect(void);
// Pauses then requests process shutdown. Does not unload live guest code/memory.
void nx2ipa_stop(void);
#endif
