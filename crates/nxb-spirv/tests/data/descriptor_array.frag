#version 450
layout(set = 0, binding = 1) uniform texture2D tex[3];
layout(set = 0, binding = 2) uniform sampler s;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 c;
void main() { c = texture(sampler2D(tex[2], s), uv); }
