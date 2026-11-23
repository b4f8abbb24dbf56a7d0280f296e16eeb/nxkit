// The host kernels of transform feedback captures (`feedback/run.rs`). One thread each: a capture has a few thousand
// records at most.
#include <metal_stdlib>
using namespace metal;

// The capture block of `main0_gs` at buffer(13) (`nxb-xlate` `msl::variant::vertex`).
struct gs_capture { device uint* dst; uint4 info; uint stride; short slot[144]; };
// The capture block of `main0_tf` at buffer(14) (`nxb-xlate` `msl::variant::tf`).
struct tf_capture { uint4 offset; uint4 args; device const uint* order; uint first, count, base_instance, pad; ulong reserved; };
// `vb_info` of a geometry kernel: vertices per instance and instances, then the primitives.
struct vb_info_t { uint4 m0; uint4 m1; uint4 m2[32]; uint4 m3[32]; };

struct job {
    device uint* state;        // the running control state: four byte offsets, records, 1, 0, 0
    device const uint* load;   // a control block that a resume loads, or null
    device const uint* src;    // the indirect draw arguments of the draw, or null
    device uint* save;         // a control block that an end or a pause saves into, or null
    device gs_capture* gs;     // the vertex capture of a geometry shader, or null
    device vb_info_t* vb_info; // the info of the geometry kernel, or null
    device uint* draw_vs;      // the draw of the vertices: count, instances, first, base instance
    device uint* dispatch;     // the threadgroups of the geometry kernel
    device const uint* ib;     // the index lists of the geometry kernel, or null
    device uint* order;        // the emitted vertices in capture order, or null
    device tf_capture* tf;     // the block of `main0_tf`
    device uint* draw_tf;      // the draw of `main0_tf`
    uint4 stride;              // the bytes of a record in each buffer, 0 for a buffer without capture
    uint4 capacity;            // the bytes of each buffer
    uint begin, count, first, instances;
    uint base_instance, cap, vpp, invocations; // cap: the most vertices the capture holds; vpp: vertices of a primitive
    uint index_stride, out_topology, group, pad;
};

// The counts of the draw, and what the vertex capture and the geometry kernel need of them.
kernel void fb_setup(device job& j [[buffer(0)]]) {
    uint n = j.count, inst = j.instances, first = j.first, base = j.base_instance;
    if (j.src) { n = j.src[0]; inst = j.src[1]; first = j.src[2]; base = j.src[3]; }
    // The vertex capture and the kernel output hold `cap` vertices.
    if (j.gs) { n = min(n, inst == 0 ? 0u : j.cap / inst); }
    n -= n % max(j.vpp, 1u);
    j.draw_vs[0] = n; j.draw_vs[1] = inst; j.draw_vs[2] = first; j.draw_vs[3] = base;
    if (j.gs) {
        j.gs->info = uint4(first, n, base, inst);
        uint prims = n / max(j.vpp, 1u);
        j.vb_info->m0 = uint4(n, inst, 0, 0);
        j.vb_info->m1 = uint4(prims, 0, 0, 0);
        j.dispatch[0] = (prims + j.group - 1) / j.group;
        j.dispatch[1] = prims == 0 ? 0 : inst;
        j.dispatch[2] = prims == 0 ? 0 : j.invocations;
    }
}

// The state step, the records of the capture and the save.
kernel void fb_finish(device job& j [[buffer(0)]]) {
    if (j.begin) { for (uint i = 0; i < 8; i++) j.state[i] = i == 5 ? 1u : 0u; }
    else if (j.load) { for (uint i = 0; i < 8; i++) j.state[i] = j.load[i]; }
    uint n = j.draw_vs[0], inst = j.draw_vs[1];
    uint records, per;
    if (j.ib) {
        // The emitted vertices in the order of the index lists. A strip becomes its primitives, as a capture of
        // the console gives them.
        per = j.out_topology == 1 ? 1u : (j.out_topology == 6 ? 2u : 3u);
        uint lists = (n / max(j.vpp, 1u)) * inst * j.invocations;
        uint r = 0;
        for (uint l = 0; l < lists; l++) {
            device const uint* ib = j.ib + l * j.index_stride;
            uint run = 0;
            uint v[3] = {0, 0, 0};
            for (uint k = 0; k < j.index_stride; k++) {
                uint x = ib[k];
                if (x == 0xffffffffu) { run = 0; continue; }
                if (per == 1) { j.order[r++] = x; continue; }
                v[0] = v[1]; v[1] = v[2]; v[2] = x; run++;
                if (per == 2 && run >= 2) { j.order[r++] = v[1]; j.order[r++] = v[2]; }
                if (per == 3 && run >= 3) {
                    bool odd = (run & 1) == 0;
                    j.order[r++] = odd ? v[1] : v[0];
                    j.order[r++] = odd ? v[0] : v[1];
                    j.order[r++] = v[2];
                }
            }
        }
        records = r;
    } else {
        per = max(j.vpp, 1u);
        records = n * inst;
    }
    for (uint b = 0; b < 4; b++) {
        if (j.stride[b]) {
            uint used = min(j.capacity[b], j.state[b]);
            records = min(records, (j.capacity[b] - used) / j.stride[b]);
        }
    }
    records -= records % per;
    j.tf->offset = uint4(j.state[0], j.state[1], j.state[2], j.state[3]);
    j.tf->args = uint4(records, 1, 0, 0);
    if (j.ib) {
        j.tf->order = j.order; j.tf->first = 0; j.tf->count = records; j.tf->base_instance = 0;
        j.draw_tf[0] = records; j.draw_tf[1] = 1; j.draw_tf[2] = 0; j.draw_tf[3] = 0;
    } else {
        j.tf->order = nullptr; j.tf->first = j.draw_vs[2]; j.tf->count = n; j.tf->base_instance = j.draw_vs[3];
        for (uint i = 0; i < 4; i++) j.draw_tf[i] = j.draw_vs[i];
    }
    for (uint b = 0; b < 4; b++) j.state[b] += records * j.stride[b];
    j.state[4] += records; j.state[5] = 1; j.state[6] = 0; j.state[7] = 0;
    if (j.save) { for (uint i = 0; i < 8; i++) j.save[i] = j.state[i]; }
}
