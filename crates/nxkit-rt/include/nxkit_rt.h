/* The C interface of libnxkit_rt.dylib, the nxkit runtime for the host.
 * The bits of the service mask follow Service::ALL in titles/nxtitle. */
#ifndef NXKIT_RT_H
#define NXKIT_RT_H

#include <stddef.h>
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

/* Like nxkit_hle_find, for the functions that need the big host stack (HLE_FORCE_BIG in the engine).
 * The host runs the function it gets on its big stack. nxkit_hle_find never returns such a function. */
const void *nxkit_hle_find_big(const char *name);

/* The callbacks of the host that the audio service needs. Every field is required. */
typedef struct NxkitAudioHost {
    uintptr_t (*guest_sym)(const char *name);                  /* the SDK's own function by mangled name, or 0 */
    void (*bind_system_event)(uint64_t addr, int32_t managed); /* guest SystemEvent to a kernel event */
    void (*signal_system_event)(uint64_t addr);
    uint32_t (*dup_readable_handle)(uint32_t handle);
    uint64_t (*spawn_guest_thread)(void (*entry)(uint64_t), uint64_t arg, int32_t priority, size_t stack);
    void (*join_guest_thread)(uint64_t thread);
    void (*fatal)(const char *msg);                            /* does not return */
} NxkitAudioHost;

typedef struct NxkitAudioConfig {
    float master_volume; /* 1.0 leaves the mix unchanged */
    int32_t silent_sink; /* 1 opens no output device */
} NxkitAudioConfig;

/* Gives the audio service its callbacks and settings. Call it once, before the game starts. cfg may be NULL.
 * Returns 1 on success, 0 when a callback is missing or it was called before. */
int32_t nxkit_audio_init(const NxkitAudioHost *host, const NxkitAudioConfig *cfg);

/* The kernel service: the Horizon thread model. Enable NXKIT_SERVICE_KERNEL in nxkit_init, call
 * nxkit_kernel_init once, then call nxkit_svc from the gate of every svc. */

/* Runs the guest code of a thread on the calling host thread and returns when the guest thread ends. */
typedef void (*nxkit_thread_entry)(uint64_t entry, uint64_t arg, uint64_t stack_top, void *user);

typedef struct nxkit_role_rule {
    const char *name;     /* NULL matches any name */
    uint32_t name_match;  /* 1 the whole name, 2 the start, 3 anywhere, other: any name */
    int32_t prio_min;     /* the priority is not checked when prio_min > prio_max */
    int32_t prio_max;
    uint64_t entry;       /* guest address of the entry function, 0 for any */
    uint32_t role;        /* 0 render, 1 audio, 2 main, 3 loading, 4 streaming, 5 worker */
} nxkit_role_rule;

typedef struct nxkit_module {
    uint64_t base;   /* address of the text segment */
    uint64_t text;
    uint64_t rodata;
    uint64_t data;
    uint64_t bss;
} nxkit_module;

/* The guest's memory. Addresses are host addresses inside one reservation. */
typedef struct nxkit_memory_config {
    uint64_t base;            /* start of the reservation */
    uint64_t size;            /* size of the reservation */
    uint64_t front_size;      /* front region: modules, thread-local area, shared memory area */
    uint64_t stack_size;      /* stack region */
    uint64_t alias_size;      /* alias region, at the end of the reservation */
    uint64_t app_pool;        /* memory pool of the application, bytes (GetInfo TotalMemorySize) */
    uint64_t used_memory;     /* what the process uses besides its heap, bytes */
    uint64_t system_resource; /* system resource size of the NPDM, or 0 */
    uint64_t program_id;
    uint64_t tls_addr;        /* thread-local area in the front region, readable and writable, 0x4000 aligned */
    uint64_t tls_size;        /* the first 0x4000 bytes are the exception context, each thread takes 0x4000 */
    uint64_t shared_addr;     /* area behind shared memory in the front region, readable and writable */
    uint64_t shared_size;
    uint64_t main_stack_addr; /* stack of the main thread inside the stack region */
    uint64_t main_stack_size; /* 0 for none */
    const nxkit_module *modules;
    size_t module_count;
    uint32_t map_limit;       /* non-zero: MapPhysicalMemory counts against the pool */
} nxkit_memory_config;

typedef struct nxkit_kernel_config {
    nxkit_thread_entry thread_entry; /* without it CreateThread is not answered */
    void *user;
    uint32_t core_rules;             /* non-zero turns the core rules of Horizon on */
    const nxkit_role_rule *roles;    /* the title's role rules, or NULL */
    size_t role_count;
    int32_t main_thread_priority;    /* NPDM main_thread priority 0..63, or -1 when the host has none */
    int32_t main_thread_core;        /* NPDM main_thread core 0..3, or -2 for the process default */
    uint64_t main_thread_stack_size; /* NPDM main_thread stack size, bytes */
    uint64_t process_id;             /* GetProcessId, 0 for the default 0x80 */
    void (*process_exit)(uint32_t code); /* ends the process for ExitProcess and Break, or NULL for exit(code) */
    const nxkit_memory_config *memory; /* NULL: no memory svc, GetInfo or IPC svc */
} nxkit_kernel_config;

/* Creates the process-wide kernel. 0 on success, 1 when it exists already, -1 for a bad configuration.
 * Guest addresses must be host addresses that stay mapped. The first svc of the main thread attaches it with the
 * priority, core and stack size above, and without a priority (-1) it cannot attach. */
int32_t nxkit_kernel_init(const nxkit_kernel_config *config);

/* Runs svc `number` with x0 to x7 in regs (read and written). Returns 1 when nxkit answered and 0 when the host
 * goes on with its own handler. After ExitThread (0x0A) it returns 1 and the host ends the host thread. */
int32_t nxkit_svc(uint32_t number, uint64_t *regs);

/* Names a thread so its role is chosen again. Returns the Horizon result code. */
uint32_t nxkit_kernel_set_thread_name(uint32_t handle, const char *name);

/* Writes the per-thread counters as text. Returns the length the whole text needs, without the NUL. */
size_t nxkit_kernel_dump_stats(char *buf, size_t capacity);

/* The thread-local region (tpidrro_el0) and TLS block (tpidr_el0) of the calling guest thread. The host sets both
 * registers from them before it runs guest code. 0 on success, -1 without a kernel, thread or memory model. */
int32_t nxkit_kernel_thread_tls(uint64_t *tlr, uint64_t *tls);

/* The SDK take-overs of the kernel service: getentropy, the csrng bytes, getenv and StandardAllocator::Allocate.
 * They are found with nxkit_hle_find when the kernel service is on. The names from nxkit_kernel_forced_names must
 * win over the SDK's own definition. */

/* Finds a function of the game's own code by its Switch mangled name. It returns the SDK's definition and never
 * the nxkit export of the same name. NULL means that the symbol does not exist. */
typedef const void *(*nxkit_guest_sym)(const char *name);

/* Sets the lookup. Once, before the game starts. 0 on success, 1 when it is set already. */
int32_t nxkit_kernel_set_guest_sym(nxkit_guest_sym lookup);

typedef struct nxkit_os_config {
    const char *const *env_names;  /* what getenv answers, env_count names */
    const char *const *env_values; /* and their values */
    size_t env_count;
    uint32_t alloc_big_align;      /* non-zero aligns StandardAllocator::Allocate of 64 KB or more to 64 KB */
} nxkit_os_config;

/* Sets the facts of the game. Once, before the game starts. 0 on success, 1 when set already, -1 when bad. */
int32_t nxkit_kernel_os_init(const nxkit_os_config *config);

/* Non-zero: getentropy, the csrng bytes and GetInfo type 11 give a fixed stream. Zero: random bytes. */
void nxkit_kernel_set_deterministic(uint32_t on);

/* The names that must win over the SDK's own definition. Writes up to capacity pointers to static strings into
 * out and returns the number of names. */
size_t nxkit_kernel_forced_names(const char **out, size_t capacity);

#ifdef __cplusplus
}
#endif

#endif
