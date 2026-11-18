#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

// Main-thread calls. The runtime retains layer; strings and error are borrowed.
// One bundled game per process. No allocation ownership crosses the C boundary.
bool nxkit_start(void *layer, const char *bundle, const char *home,
                 uint32_t maximum_fps, char *error, size_t error_capacity);
typedef struct { uint32_t id; float x; float y; } NxkitTouch;
// Current unclaimed fingers, normalized to the drawable. Borrowed for this call.
void nxkit_touches(const NxkitTouch *points, size_t count);
void nxkit_set_active(bool active);
void nxkit_resize(uint32_t width, uint32_t height);
// Complete Npad snapshot: bits A B X Y L3 R3 L R ZL ZR + - Left Up Right Down.
// Sticks use x-right/y-up normalized coordinates in [-1, 1].
void nxkit_virtual_pad(uint64_t buttons, float lx, float ly, float rx, float ry);
void nxkit_stop(void);
