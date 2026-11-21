"""Writes modules.rs: the SPIR-V test modules of this folder as words, so that no binary module is in the tree.

Usage: compile each GLSL file beside it with `glslangValidator -V -o <file>.spv <file>`, then run
`python3 -I crates/nxb-spirv/tests/data/words.py` and delete the .spv files again.
"""
import pathlib
import struct

HERE = pathlib.Path(__file__).resolve().parent
MODULES = [
    ("VERT", "basic.vert"),
    ("FRAG", "basic.frag"),
    ("ARRAYS", "arrays.frag"),
    ("DESCRIPTOR_ARRAY", "descriptor_array.frag"),
    ("RUNTIME_ARRAY", "runtime_array.frag"),
    ("IMAGE1D", "image1d.frag"),
    ("IMAGE1D_ARRAY", "image1d_array.frag"),
    ("KERNEL", "kernel.comp"),
]

out = [
    "// The SPIR-V test modules as words: the GLSL files of this folder compiled with `glslangValidator -V`.",
    "// Written by words.py; edit the GLSL file and run it again. Shared by the tests of nxb-spirv and nxb-build.",
    "",
    "/// The bytes of SPIR-V words, little-endian as a module file holds them.",
    "#[allow(dead_code)]",
    "const fn spirv_bytes<const N: usize>(words: &[u32]) -> [u8; N] {",
    "    let mut out = [0u8; N];",
    "    let mut i = 0;",
    "    while i < words.len() {",
    "        let b = words[i].to_le_bytes();",
    "        out[4 * i] = b[0];",
    "        out[4 * i + 1] = b[1];",
    "        out[4 * i + 2] = b[2];",
    "        out[4 * i + 3] = b[3];",
    "        i += 1;",
    "    }",
    "    out",
    "}",
]
for name, src in MODULES:
    data = (HERE / f"{src}.spv").read_bytes()
    words = struct.unpack(f"<{len(data) // 4}I", data)
    out += ["", f"/// `{src}`.", "#[allow(dead_code)]", f"const {name}_WORDS: &[u32] = &["]
    for i in range(0, len(words), 8):
        out.append("    " + " ".join(f"0x{w:08x}," for w in words[i : i + 8]))
    out += [
        "];",
        "#[allow(dead_code)]",
        f"pub(crate) const {name}: &[u8] = &spirv_bytes::<{{ {name}_WORDS.len() * 4 }}>({name}_WORDS);",
    ]
(HERE / "modules.rs").write_text("\n".join(out) + "\n")
