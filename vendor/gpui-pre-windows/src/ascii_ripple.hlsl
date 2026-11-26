// claudesktop patch: 首页 ASCII 波动字阵。
//
// CPU 侧（`apps/desktop/src/app/ripple.rs`）跑波动方程，把每个格子压成 4 个字节：
//   R = 字形下标（0..62）、G = 该格的不透明度、B = ink→wave 的混合、
//   A = 高 3 bit 棱镜强度 + 低 5 bit 色相。
// 这里只负责按格子铺字形、上色、叠指针光晕，一个实例 = 一屏字阵。
//
// 与 `vendor/gpui-pre/src/scene.rs` 的 `AsciiRippleParams` / `AsciiRippleUniform`
// 逐字节对应（176 字节）。
#include "prism_common.hlsl"

struct AsciiRippleParams {
    // cell_w, cell_h, atlas_cell（一格字形的像素宽）, glyph_count
    float4 grid;
    // 静止态字色 rgb + 不透明度
    float4 ink;
    // 波面颜色 rgb + 混合增益
    float4 wave;
    // 指针光晕圆心 x / y（相对 bounds 左上、物理像素）、半径、强度
    float4 aura;
    // 色散、色相偏转、半调、对比
    float4 optics;
    // 秒、深色程度、整体强度、云雾相位
    float4 timing;
    // 云雾强度、光晕抬升、呼吸、棱镜
    float4 nebula;
    // 图集像素宽、高、占位、占位
    float4 atlas_size;
};

struct AsciiRipple {
    uint order;
    Bounds bounds;
    Bounds content_mask;
    AsciiRippleParams params;
    float3 pad;
};

StructuredBuffer<AsciiRipple> ripples: register(t1);
// 每格 4 字节的单元格数据，用 Load 直接按格子取，不走采样器。
Texture2D<float4> t_cells: register(t0);
// 字形图集：62 格横向拼在一张 R8 贴图上。
Texture2D<float> t_atlas: register(t2);
SamplerState s_atlas: register(s0);

struct AsciiRippleVertexOutput {
    nointerpolation uint ripple_id: TEXCOORD0;
    float4 position: SV_Position;
    float4 clip_distance: SV_ClipDistance;
};

struct AsciiRippleFragmentInput {
    nointerpolation uint ripple_id: TEXCOORD0;
    float4 position: SV_Position;
};

AsciiRippleVertexOutput ascii_ripple_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = unit_vertex_for(vertex_id);
    uint ripple_id = batch_start_index + instance_id;
    AsciiRipple ripple = ripples[ripple_id];
    float2 position = unit_vertex * ripple.bounds.size + ripple.bounds.origin;

    AsciiRippleVertexOutput output;
    output.ripple_id = ripple_id;
    output.position = to_device_position(position);
    output.clip_distance = distance_from_clip_rect_impl(position, ripple.content_mask);
    return output;
}

float4 ascii_ripple_fragment(AsciiRippleFragmentInput input): SV_Target {
    AsciiRipple ripple = ripples[input.ripple_id];
    if (ripple.params.timing.z <= 0.002 || ripple.params.grid.w < 1.) {
        discard;
    }

    float2 cell = max(ripple.params.grid.xy, float2(1., 1.));
    float2 size = max(ripple.bounds.size, cell);
    float2 local = input.position.xy - ripple.bounds.origin;
    if (local.x < 0. || local.y < 0. || local.x >= size.x || local.y >= size.y) {
        discard;
    }

    // 单元格数据：一次 Load，不取样，格子边界干净。
    uint cells_width = 0;
    uint cells_height = 0;
    t_cells.GetDimensions(cells_width, cells_height);
    float2 cell_uv = local / cell;
    int2 cell_index = int2(floor(cell_uv));
    if (cell_index.x < 0 || cell_index.y < 0 || cell_index.x >= (int)cells_width ||
        cell_index.y >= (int)cells_height) {
        discard;
    }
    float4 data = t_cells.Load(int3(cell_index, 0));
    if (data.g <= 0.004) {
        discard;
    }

    // 字形：R 通道 0..255 → 0..glyph_count-1，图集里横向第 n 格。
    float glyph_count = max(ripple.params.grid.w, 1.);
    float atlas_cell = max(ripple.params.grid.z, 1.);
    float glyph = floor(data.r * 255. + 0.5);
    float2 within = frac(cell_uv);

    // 取图集里那一格：用纹素中心、POINT 采样，避免相邻字形渗色。
    float2 atlas_px = float2(max(ripple.params.atlas_size.x, 1.), max(ripple.params.atlas_size.y, 1.));
    // 用 POINT 采样 + 纹素中心：`within` 满格映射到 `atlas_cell` 个纹素，
    // 右/下边界收半个纹素，最右一列不会被压窄，也不会串到下一个字形。
    float2 atlas_uv = float2(
        (glyph * atlas_cell + min(saturate(within.x) * atlas_cell, atlas_cell - 0.5)) / atlas_px.x,
        min(saturate(within.y) * atlas_px.y, atlas_px.y - 0.5) / atlas_px.y
    );
    float coverage = t_atlas.Sample(s_atlas, atlas_uv);
    if (coverage <= 0.004) {
        discard;
    }

    // 打包在 A 通道里的棱镜强度与色相。
    float packed = floor(data.a * 255. + 0.5);
    float prism = floor(packed / 32.) / 7.;
    float hue = frac(packed / 32.);

    // 上色：静止态是墨色，波峰处变成光谱色。
    float wave_mix = saturate(data.b) * saturate(ripple.params.wave.a);
    float3 spectral = hsv_to_rgb(float3(frac(hue + ripple.params.optics.y), 0.58, 0.80));
    float3 color = lerp(
        ripple.params.ink.rgb,
        spectral * 0.65 + ripple.params.wave.rgb * 0.55,
        wave_mix
    );

    // 棱镜色散：按色相把 R / B 通道拉开一点，做出玻璃边。
    float dispersion = prism * ripple.params.optics.x;
    if (dispersion > 0.01) {
        color.r = saturate(color.r + dispersion * 0.10 * (hue - 0.5));
        color.b = saturate(color.b + dispersion * 0.10 * (0.5 - hue));
    }

    // 指针光晕：光晕里字形亮一档，像被手指扫过。
    float2 aura_delta = local - ripple.params.aura.xy;
    float aura = saturate(1. - length(aura_delta) / max(ripple.params.aura.z, 1.));
    aura = pow(aura, 2.) * ripple.params.aura.w;
    color = lerp(color, saturate(color * 1.25 + 0.16), aura);

    // 对比：把弱能量压下去，字阵才不像一层均匀噪点。
    float contrast = lerp(1.6, 0.7, saturate(ripple.params.optics.w));
    float alpha = saturate(pow(saturate(data.g), contrast));
    alpha *= coverage * ripple.params.ink.a * ripple.params.timing.z;
    alpha *= 1. + aura * 1.1;

    // 云雾：整层极缓的起伏，静止时也有一点点呼吸（由 CPU 侧控制强度）。
    float nebula = ripple.params.nebula.x * (0.5 + 0.5 * value_noise(local * 0.0025 + ripple.params.timing.w));
    alpha *= lerp(1., nebula, 0.25 * saturate(ripple.params.nebula.z));

    if (alpha <= 0.002) {
        discard;
    }
    return float4(color * alpha, alpha);
}
