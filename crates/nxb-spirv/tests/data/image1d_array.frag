#version 450
layout(set = 0, binding = 0) uniform sampler1DArray lut;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 c;
void main() { c = texture(lut, uv); }
