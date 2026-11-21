#version 450
layout(set = 0, binding = 0) readonly buffer B { vec4 v[4]; } b;
layout(set = 0, binding = 1) uniform texture2D tex;
layout(set = 0, binding = 2) uniform sampler s;
layout(set = 0, binding = 5) uniform sampler2D unused;
layout(set = 1, binding = 0) uniform sampler2D used;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 c;
void main() { c = b.v[0] + texture(sampler2D(tex, s), uv) + texture(used, uv); }
