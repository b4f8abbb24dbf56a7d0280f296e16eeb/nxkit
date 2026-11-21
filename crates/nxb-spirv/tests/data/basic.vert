#version 450
layout(set = 0, binding = 1) uniform U { mat4 mvp; vec4 tint[2]; } u;
layout(push_constant) uniform P { vec4 off; } p;
layout(location = 0) in vec4 pos;
layout(location = 1) in vec2 uv;
layout(location = 0) out vec2 v_uv[2];
void main() { v_uv[0] = uv; v_uv[1] = uv * 2.0; gl_Position = u.mvp * pos + p.off + u.tint[1]; }
