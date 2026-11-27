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
    float inset;
    float2 pad;
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

    // `rim.bounds` 四边向卡片外扩了 `rim.inset` 像素，这里把半宽半高缩回卡片真实边缘，
    // 这样描边环（sdf == 0）外半截和柔和微光都不会被 quad 边界裁掉。
    float2 full_half = max(rim.bounds.size * 0.5, float2(0.5, 0.5));
    float2 center_to_point = (input.position.xy - rim.bounds.origin) - full_half;
    float2 card_half = max(full_half - rim.inset, float2(0.5, 0.5));
    float corner_radius = rim_corner_radius(center_to_point, rim.corner_radii);
    float sdf = rounded_rect_sdf(center_to_point, card_half, corner_radius);

    // 指针归一化半径：只渲染光谱覆盖的区域，离开半径外直接丢弃。
    float dist_to_pointer = length(input.position.xy - rim.pointer);
    float r = dist_to_pointer / max(rim.radius, 1.);
    if (r >= 1.) {
        discard;
    }

    // 完整描边 + 随 inset 归一化的柔和玻璃微光（到外扩边界处衰减到近零，无硬切边）。
    float stroke_cov = saturate(rim.stroke_width * 0.5 - abs(sdf) + 0.5);
    float halo_scale = max(rim.inset * 0.35, 1.);
    float halo_cov = exp(-abs(sdf) / halo_scale) * (rim.dark > 0.5 ? 0.36 : 0.22);
    float shape = saturate(stroke_cov + halo_cov);
    if (shape <= 0.004) {
        discard;
    }

    float4 spec = prism_rim_sample(rim.tone_key, r, rim.dark > 0.5 ? 1. : 0.);
    float mono_mul = rim.tone_key == 5u ? 0.62 : 1.;
    float alpha = shape * spec.a * rim.alpha * mono_mul;
    if (alpha <= 0.002) {
        discard;
    }

    return float4(spec.rgb * alpha, alpha);
}
