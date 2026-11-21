#version 450
layout(set = 1, binding = 21) uniform texture1D shadow;
layout(set = 0, binding = 24) uniform sampler smp;
layout(set = 1, binding = 3) uniform sampler1D lut;
layout(set = 1, binding = 4, r32f) uniform image1D img;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 c;
void main() {
    float a = texture(sampler1D(shadow, smp), uv.x).r;
    vec4 b = texelFetch(lut, int(uv.y), 0);
    int w = textureSize(lut, 0);
    imageStore(img, int(uv.x * 4.0), vec4(a));
    c = vec4(a) + b + vec4(float(w)) + imageLoad(img, 1) + textureLod(lut, uv.y, 1.0);
}
