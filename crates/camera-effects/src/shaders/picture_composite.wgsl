// Appended to mask_composite.wgsl: uses its bindings 0-4, VertexOutput and
// foreground_alpha.

struct PictureUniforms {
    scale: vec2<f32>,
    offset: vec2<f32>,
};

@group(0) @binding(5) var picture_tex: texture_2d<f32>;
@group(0) @binding(6) var<uniform> picture_uniforms: PictureUniforms;

// The person over a still picture. Edge colours are unmixed from the room
// exactly as fs_cutout does, then blended over the picture.
@fragment
fn fs_picture(in: VertexOutput) -> @location(0) vec4<f32> {
    let sharp = textureSample(sharp_tex, tex_sampler, in.uv);
    let alpha = foreground_alpha(in.uv, sharp.rgb);
    let room = textureSampleLevel(blurred_tex, tex_sampler, in.uv, 0.0);
    var color = sharp.rgb;
    if alpha > 0.0 && alpha < 1.0 && room.a > 0.01 {
        let unmixed = (sharp.rgb - (1.0 - alpha) * room.rgb / room.a) / alpha;
        let shift = clamp(unmixed - sharp.rgb, vec3<f32>(-0.25), vec3<f32>(0.25));
        color = clamp(sharp.rgb + shift * smoothstep(0.2, 0.6, alpha), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let picture_uv = in.uv * picture_uniforms.scale + picture_uniforms.offset;
    let picture = textureSampleLevel(picture_tex, tex_sampler, picture_uv, 0.0);
    return vec4<f32>(mix(picture.rgb, color, alpha), sharp.a);
}
