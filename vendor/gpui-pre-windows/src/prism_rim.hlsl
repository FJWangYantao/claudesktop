// claudesktop patch: 输入框棱镜描边。
//
// 和上游 `shaders.hlsl` 分开成独立文件（而不是往上游那份里塞）：三个效果各一个文件，
// 一来三个入口都从 `t1` 读自己的 StructuredBuffer，放进同一个编译单元会撞寄存器；
// 二来出问题时 `git diff` 只落在这些文件上，升级 gpui 重打补丁也好对照。
#include "prism_common.hlsl"

// 与 `vendor/gpui-pre/src/scene.rs` 的 `PrismRim` 逐字节对应（96 字节）。
struct PrismRim {
    uint order;
    Bounds bounds;
    Bounds content_mask;
    Corners corner_radii;
    float2 pointer;
    float radius;
    float stroke_width;
    float alpha;
    uint dark;
    uint tone_key;
    uint mode;
    float3 pad;
};

StructuredBuffer<PrismRim> rims: register(t1);

struct PrismRimVertexOutput {
    nointerpolation uint rim_id: TEXCOORD0;
    float4 position: SV_Position;
    float4 clip_distance: SV_ClipDistance;
};

struct PrismRimFragmentInput {
    nointerpolation uint rim_id: TEXCOORD0;
    float4 position: SV_Position;
};

PrismRimVertexOutput prism_rim_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = unit_vertex_for(vertex_id);
    uint rim_id = batch_start_index + instance_id;
    PrismRim rim = rims[rim_id];
    float2 position = unit_vertex * rim.bounds.size + rim.bounds.origin;

    PrismRimVertexOutput output;
    output.rim_id = rim_id;
    output.position = to_device_position(position);
    output.clip_distance = distance_from_clip_rect_impl(position, rim.content_mask);
    return output;
}

/// 圆角矩形 SDF，负值在内部。半径按象限取，和上游 `pick_corner_radius` 一个意思。
float rim_corner_radius(float2 center_to_point, Corners corners) {
    if (center_to_point.x < 0.) {
        return center_to_point.y < 0. ? corners.top_left : corners.bottom_left;
    }
    return center_to_point.y < 0. ? corners.top_right : corners.bottom_right;
}

float rounded_rect_sdf(float2 center_to_point, float2 half_size, float radius) {
    float r = max(radius, 0.);
    float2 q = abs(center_to_point) - half_size + r;
    return length(max(q, 0.)) + min(max(q.x, q.y), 0.) - r;
}

float4 prism_rim_fragment(PrismRimFragmentInput input): SV_Target {
    PrismRim rim = rims[input.rim_id];
    if (rim.alpha <= 0.002) {
        discard;
    }

    float2 half_size = max(rim.bounds.size * 0.5, float2(0.5, 0.5));
    float2 center_to_point = (input.position.xy - rim.bounds.origin) - half_size;
    float corner_radius = rim_corner_radius(center_to_point, rim.corner_radii);
    float sdf = rounded_rect_sdf(center_to_point, half_size, corner_radius);

    // 只在描边环上画：环心在 sdf == 0，宽度 stroke_width，边缘 1px 抗锯齿。
    float coverage = saturate(rim.stroke_width * 0.5 - abs(sdf) + 0.5);
    if (coverage <= 0.002) {
        discard;
    }

    // 指针光谱：距离归一化后既决定「有没有颜色」也决定色相偏移，
    // 指针离得远时只剩描边自身的淡墨色。
    float2 to_pointer = input.position.xy - rim.pointer;
    float distance_to_pointer = length(to_pointer);
    float proximity = saturate(1. - distance_to_pointer / max(rim.radius, 1.));
    proximity *= proximity;

    float angle = atan2(to_pointer.y, to_pointer.x) / 6.2831853 + 0.5;
    float hue_t = frac(angle + distance_to_pointer / max(rim.radius, 1.) * 0.35);
    float3 spectral = prism_palette(rim.tone_key, hue_t, rim.dark);

    // 底色：浅色底用淡墨，深色底用亮白；指针越近越吃光谱色。
    float3 base = rim.dark > 0.5 ? float3(0.97, 0.96, 0.95) : float3(0.20, 0.19, 0.18);
    float3 color = lerp(base, spectral, saturate(proximity * 0.9 + 0.06));
    float alpha = coverage * rim.alpha * (0.34 + 0.66 * proximity);

    // mode == 1：底部一层向内反射，让卡片下沿像有光从桌面反上来。
    if (rim.mode == 1u) {
        float2 normalized = (input.position.xy - rim.bounds.origin) / max(rim.bounds.size, float2(1., 1.));
        float from_bottom = saturate(1. - normalized.y);
        float inside = saturate(-sdf / 22.);
        float reflect = from_bottom * from_bottom * inside * proximity * 0.45;
        color = lerp(color, spectral, saturate(reflect));
        alpha = saturate(alpha + reflect * 0.35);
    }

    return float4(color * alpha, alpha);
}
