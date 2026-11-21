#version 450
layout(set = 0, binding = 2) uniform sampler2D tex;
layout(location = 0) in vec2 v_uv[2];
layout(location = 0) out vec4 c0;
layout(location = 1) out vec4 c1;
void main() { c0 = texture(tex, v_uv[0]); c1 = vec4(min(v_uv[1].x, 0.5)); }
