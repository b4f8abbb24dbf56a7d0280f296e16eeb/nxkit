# AIR conventions

A front end that writes AIR must obey the conventions below. They come from observations of Apple's tool output
(toolchain 32023.922) on small samples and on 75 reference `.ll` files that clang made on the local machine.
This document is a compatibility note. It contains no Apple code.

## Module header

- The IR is LLVM with typed pointers (`float addrspace(1)*`, `i8*`) and LLVM 14 attributes (`readonly`, `argmemonly`, `nocapture`, `noundef`, `mustprogress`).
- `air-as` changes text to bitcode. `air-opt -S` changes bitcode to text. The toolchain has no `llvm-dis`.
- The data layout string is the same on macOS and iOS. It starts with `e-p:64:64:64-i1:8:8` and gives the vector alignments up to `v1024:1024:1024`.
- The triple is `air64_v<AIR major><AIR minor>-apple-<os><version>`:

| target | triple | AIR version | language | metallib file version |
|---|---|---|---|---|
| macOS 13.0 | `air64_v25-apple-macosx13.0.0` | 2.5.0 | Metal 3.0.0 | 2.7 |
| macOS 15.0 | `air64_v27-apple-macosx15.0.0` | 2.7.0 | Metal 3.2.0 | 2.8 |
| macOS 27.2 (SDK default) | `air64_v29-apple-macosx27.2.0` | 2.9.0 | Metal 4.1.0 | 2.9 |
| iOS 16.0 | `air64_v25-apple-ios16.0.0` | 2.5.0 | Metal 3.0.0 | 2.7 |
| iOS 18.0 (engine shaders) | `air64_v27-apple-ios18.0.0` | 2.7.0 | Metal 3.2.0 | 2.8 |

- Module flags: `SDK Version`, `wchar_size 4`, `frame-pointer 2`, and the binding limits (31 buffers of each type, 128 textures, 8 read-write textures, 16 samplers).
- Named metadata: `!air.vertex`, `!air.fragment`, `!air.kernel`, `!air.mesh`, `!air.object` (one entry for each entry point),
  `!air.compile_options`, `!air.function_constants`, `!llvm.ident`, `!air.version`, `!air.language_version`, `!air.source_file_name`.
- `!air.compile_options` holds one string of each pair: denorms, fast math (the engine shaders use `disable`), framebuffer fetch.

## Entry points

- An entry has the tuple: function, outputs, inputs, and optional more data. A kernel has empty outputs.
- Each IR argument has one input tuple: argument index, kind, kind fields, `air.arg_type_name`, `air.arg_name`, and optional `air.arg_unused`.
- Vertex outputs: `air.position`, `air.vertex_output` with a generated name, `air.point_size`, `air.clip_distance`, `air.invariant`.
- The generated name has the form `generated(<length><name><Itanium type>)`. It connects a vertex output to a fragment input.
  Thus the two sides must make the same string.

## Resource bindings

- **Buffer:** `air.buffer`, optional `air.buffer_size` (for references, not for pointers), `air.location_index`, access, `air.address_space`, optional `air.struct_type_info`, type size and alignment.
- **Buffer arguments** have the attribute `"air-buffer-no-alias"`.
- **Texture:** `air.texture`, location index and count, access. An array of four 2D textures is one IR argument with location count 4.
- **Sampler:** `air.sampler` with a location index.
- **Struct layout:** `air.struct_type_info` is a flat list of 5-tuples: offset, size, array length, type, name.
- **Argument buffer:** `air.indirect_buffer` with members marked `air.indirect_argument`. Scalars in it use `air.indirect_constant`.
- **Function constant gate:** a prefix `air.function_constant` with a predicate global.

## Address spaces

| value | space |
|---|---|
| 0 | thread or private (allocas, byval arrays) |
| 1 | device (buffers, texture handles) |
| 2 | constant (constant buffers, sampler handles, function-constant globals) |
| 3 | threadgroup (also mesh grid properties) |
| 4 | imageblock handle |
| 6 | object data and payload |
| 7 | mesh handle |

The samples did not show the value 5 or values more than 7.

## Function constants

- Each constant has a global in address space 2 with the section `"air.fc_initializer"`.
- A constructor in the section `"air.static_init"` fills a shadow global. `@llvm.global_ctors` lists the constructor.
- `!air.function_constants` has one entry for each constant: global, MSL type, name, index. `metallib` adds a "required" flag.

## Intrinsics

- Names have the form `air.<operation>[.<overload>]` with LLVM type suffixes (`f32`, `v4f32`, `i32`, `v2f16`).
  The sign is in the name (`air.min.u.i32`). Fast-math variants have the prefix `fast_` (`air.fast_sqrt.f32`).
- Conversions: `air.convert.<destination kind>.<destination type>.<source kind>.<source type>`, kinds `f`, `s` or `u`.
- Texture operations: `air.sample_texture_2d.v4f32` returns a vector and a status byte. Also `read_texture_*`, `write_texture_*`, `gather_*`.
- Synchronization: `air.wg.barrier` for `threadgroup_barrier`. SIMD: `air.simd_shuffle`, `air.simd_ballot`. Discard: `air.discard_fragment`.
- `simd_shuffle` is `air.simd_shuffle.<sign>.<type>` with `s` or `u` for integers and no sign for floats. The lane is an `i16`.
  Apple's front end writes a `freeze` of the data operand before the call. The operand and the result of `simd_any` and `simd_all` are `i1`.
- `mulhi` is `air.mul_hi.s.<type>` or `air.mul_hi.u.<type>`.
- The declaration attributes are important: `convergent` for barriers and for sample operations with implicit derivatives, `readnone` for math. Copy them from Apple output for each intrinsic.

## Function attributes

Under fast math (not used by the engine, both targets compile with relaxed math), the definitions have `"approx-func-fp-math"`, `"no-infs-fp-math"`, `"no-nans-fp-math"`,
`"no-signed-zeros-fp-math"` and `"unsafe-fp-math"` set to `true`, and arithmetic has `fast` flags.
All definitions have `"frame-pointer"="all"`, `"min-legal-vector-width"`, `"no-builtins"`, `"no-trapping-math"="true"` and `"stack-protector-buffer-size"="8"`.

## Relaxed math and iOS differences

Both conventions use relaxed math (`-fmetal-math-mode=relaxed`). They have no `nnan` and `ninf` flags and no `no-infs` and `no-nans` attributes.
Vectors have the `omnipotent char` TBAA node (Metal before version 4). Texture `read` has no read sampler and no offset operand.
The builtin intrinsics, the module flags and the compile options are the same as on macOS.
