#version 450
layout(set = 0, binding = 0) readonly buffer B { vec4 v[]; } b;
layout(location = 0) out vec4 c;
void main() { c = b.v[b.v.length() - 1]; }
