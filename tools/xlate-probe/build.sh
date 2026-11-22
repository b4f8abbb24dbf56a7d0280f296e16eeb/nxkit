#!/bin/sh
# Build the dev-only oracle probe: a copy of the translator source plus Probe.cs (text dumps per phase).
# Usage: build.sh <engine checkout dm dir> <work dir>   (needs DOTNET_ROOT, default $HOME/.dotnet, and the offline NuGet cache)
# Run:   XLATE_DUMP=<dir> $DOTNET_ROOT/dotnet <work>/bin/dmsh_xlate.dll <jobs.json> <outdir>
# The dumps are <key>.<phase>.txt. Every translation attempt of a job appends a section starting with "== attempt".
# Phases are added by hooking Probe.Write into the translator copy (see the hooks listed in docs/translator-port.md).
set -eu
DM=$1; W=$2
export DOTNET_ROOT=${DOTNET_ROOT:-$HOME/.dotnet} NUGET_PACKAGES=${NUGET_PACKAGES:-$HOME/.nuget/packages} DOTNET_NOLOGO=1
mkdir -p "$W/xl"
cp -R "$DM/third_party/ryujinx" "$W/ryu"
cp "$DM/gfx/tools/dmsh_xlate/Program.cs" "$DM/gfx/tools/dmsh_xlate/dmsh_xlate.csproj" "$W/xl/"
cp "$(dirname "$0")/Probe.cs" "$W/ryu/src/Ryujinx.Graphics.Shader/Translation/Probe.cs"
perl -0pi -e 's|(            return new TranslatorContext\(address, size, localMemorySize)|            Probe.Decoded(program, size);\n$1|' "$W/ryu/src/Ryujinx.Graphics.Shader/Translation/Translator.cs"
T="$W/ryu/src/Ryujinx.Graphics.Shader/Translation/TranslatorContext.cs"
perl -0pi -e 's|(                cfgs\[i\] = ControlFlowGraph.Create\(functions\[i\].Code\);)|                Probe.Ops("emit", i, functions[i].Code);\n$1|' "$T"
perl -0pi -e 's|(                    RegisterUsage.FixupCalls\(cfg.Blocks, frus\);)|$1\n                    Probe.Blocks("cfg", i, cfg.Blocks);|' "$T"
perl -0pi -e 's|(                    Ssa.Rename\(cfg.Blocks\);)|$1\n                    Probe.Blocks("ssa", i, cfg.Blocks);|' "$T"
perl -0pi -e 's|(                    Optimizer.RunPass\(context\);)|$1\n                    Probe.Blocks("opt", i, cfg.Blocks);|' "$T"
perl -0pi -e 's|(                    TransformPasses.RunPass\(context\);)|$1\n                    Probe.Blocks("xform", i, cfg.Blocks);|' "$T"
perl -0pi -e 's|(            CodeGenParameters parameters = new CodeGenParameters\([^;]*;)|$1\n            Probe.Props(sInfo, parameters, resourceManager);|' "$T"
perl -0pi -e 's|(            StructuredProgramInfo sInfo = StructuredProgram.MakeStructuredProgram\()|            Probe.SFuncs(funcs);\n$1|' "$T"
perl -0pi -e 's|string key = \(string\)job\["key"\];|string key = (string)job["key"]; Ryujinx.Graphics.Shader.Translation.Probe.Tag = key;|' "$W/xl/Program.cs"
R="$W/ryu/src/Ryujinx.Graphics.Shader/Translation/ResourceManager.cs"
perl -0pi -e 's|(public ResourceManager\(ShaderStage stage, IGpuAccessor gpuAccessor, ResourceReservations reservations = null\)\s*\{)|$1\n            Probe.Res("new " + (int)stage + " " + (reservations != null ? 1 : 0));|' "$R"
perl -0pi -e 's|(public void SetCurrentLocalMemory\(int size, bool isUsed\)\s*\{)|$1\n            Probe.Res("local " + size + " " + (isUsed ? 1 : 0));|' "$R"
perl -0pi -e 's|(public void SetCurrentSharedMemory\(int size, bool isUsed\)\s*\{)|$1\n            Probe.Res("shared " + size + " " + (isUsed ? 1 : 0));|' "$R"
perl -0pi -e 's|(public int GetHmmaScratchMemoryId\(int subgroups\)\s*\{)|$1\n            Probe.Res("hmma " + subgroups);|' "$R"
perl -0pi -e 's|(public void SetVertexAsComputeLocalMemories\(ShaderStage stage, InputTopology inputTopology\)\s*\{)|$1\n            Probe.Res("vac");|' "$R"
perl -0pi -e 's|(public int GetConstantBufferBinding\(int slot\)\s*\{)|$1\n            Probe.Res("cb " + slot);|' "$R"
perl -0pi -e 's|(public bool TryGetStorageBufferBinding\(int sbCbSlot, int sbCbOffset, bool write, out int binding\)\s*\{)|$1\n            Probe.Res("sb " + sbCbSlot + " " + sbCbOffset + " " + (write ? 1 : 0));|' "$R"
perl -0pi -e 's|(            int arrayLength = 1,\s*bool separate = false\)\s*\{)|$1\n            Probe.Res("tex " + (int)inst + " " + (int)type + " " + (int)format + " " + (int)flags + " " + cbufSlot + " " + handle + " " + arrayLength + " " + (separate ? 1 : 0));|' "$R"
perl -0pi -e 's|(public void SetUsageFlagsForTextureQuery\(int binding, SamplerType type\)\s*\{)|$1\n            Probe.Res("q " + binding + " " + (int)type);|' "$R"
perl -0pi -e 's|(public void SetUsedConstantBufferBinding\(int binding\)\s*\{)|$1\n            Probe.Res("ucb " + binding);|' "$R"
cd "$W/xl" && "$DOTNET_ROOT/dotnet" build -c Release -p:RyuSrc="$W/ryu" -o "$W/bin"
