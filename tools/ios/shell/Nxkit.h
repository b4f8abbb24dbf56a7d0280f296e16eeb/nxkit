#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

bool nxkit_start(void *layer, const char *bundle, const char *home,
                 uint32_t maximum_fps, char *error, size_t capacity);
void nxkit_set_active(bool active);
