/* The C interface of libnxkit_rt.a, the nxkit runtime for the host.
 * The bits of the service mask follow Service::ALL in titles/nxtitle. */
#ifndef NXKIT_RT_H
#define NXKIT_RT_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define NXKIT_SERVICE_FS      (UINT64_C(1) << 0)
#define NXKIT_SERVICE_INPUT   (UINT64_C(1) << 1)
#define NXKIT_SERVICE_AUDIO   (UINT64_C(1) << 2)
#define NXKIT_SERVICE_VIDEO   (UINT64_C(1) << 3)
#define NXKIT_SERVICE_SYSTEM  (UINT64_C(1) << 4)
#define NXKIT_SERVICE_OFFLINE (UINT64_C(1) << 5)
#define NXKIT_SERVICE_PACING  (UINT64_C(1) << 6)
#define NXKIT_SERVICE_KERNEL  (UINT64_C(1) << 7)
#define NXKIT_SERVICE_GPU     (UINT64_C(1) << 8)

/* Sets the enabled services. Bits that name no service are ignored.
 * Every call replaces the mask of the earlier call. Safe from any thread. */
void nxkit_init(uint64_t services);

/* Returns the function for a Switch mangled name, or NULL.
 * NULL also means: the service of the name is not enabled, or nxkit_init was not called. */
const void *nxkit_hle_find(const char *name);

#ifdef __cplusplus
}
#endif

#endif
