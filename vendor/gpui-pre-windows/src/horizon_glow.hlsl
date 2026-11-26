// claudesktop patch: 首页地平线极光。
//
// 与 `vendor/gpui-pre/src/scene.rs` 的 `HorizonGlow` 逐字节对应（96 字节）。
#include "prism_common.hlsl"

struct HorizonGlow {
    uint order;
    Bounds bounds;
    Bounds content_mask;
    float dpr;
    float time;
    float dark;
    float reach;
    float intensity;
    float stretch;
    float presence;
    float prism;
    float surge;
    float speed;
    float spectrum;
    float semantic;
    float3 pad;
};

StructuredBuffer<HorizonGlow> horizons: register(t1);

struct HorizonGlowVertexOutput {
    nointerpolation uint glow_id: TEXCOORD0;
    float4 position: SV_Position;
    float4 clip_distance: SV_ClipDistance;
};

struct HorizonGlowFragmentInput {
    nointerpolation uint glow_id: TEXCOORD0;
    float4 position: SV_Position;
};

HorizonGlowVertexOutput horizon_glow_vertex(uint vertex_id: SV_VertexID, uint instance_id: SV_InstanceID) {
    float2 unit_vertex = unit_vertex_for(vertex_id);
    uint glow_id = batch_start_index + instance_id;
    HorizonGlow glow = horizons[glow_id];
    float2 position = unit_vertex * glow.bounds.size + glow.bounds.origin;

    HorizonGlowVertexOutput output;
    output.glow_id = glow_id;
    output.position = to_device_position(position);
    output.clip_distance = distance_from_clip_rect_impl(position, glow.content_mask);
    return output;
}

float4 horizon_glow_fragment(HorizonGlowFragmentInput input): SV_Target {
    HorizonGlow glow = horizons[input.glow_id];
    if (glow.intensity <= 0.002) {
        discard;
    }

    float2 size = max(glow.bounds.size, float2(1., 1.));
    float2 local = input.position.xy - glow.bounds.origin;

    // D3D 的 y 轴向下，而极光是从底部升起来的：这里翻一次 y，
    // 后面所有高度都按「0 = 底、1 = 顶」算。（其他两个效果不翻。）
    float2 uv = float2(local.x / size.x, 1. - local.y / size.y);
    // 逻辑像素、左下原点：噪声按这个走，换缩放比不会变粗变细。
    float2 px = float2(local.x, size.y - local.y) / max(glow.dpr, 0.5);

    float t = glow.time * glow.speed;

    // 地平线本身也跟着抖一下，不然像一条摆上去的直线。
    float horizon = 0.05 + 0.07 * fbm4(float2(px.x * 0.0016 * glow.stretch, t * 0.05));
    float above = saturate((uv.y - horizon) / max(0.9 * glow.reach, 0.02)); // 0 = 地平线，1 = 顶
    float rise = pow(1. - above, 2.4);

    // 极光的帘幕：两个不同尺度的噪声叠出竖向的丝缕。
    float curtain_a = fbm4(float2(px.x * 0.0018 * glow.stretch, t * 0.08));
    float curtain_b = fbm4(float2(px.x * 0.0042 * glow.stretch + 11.7, uv.y * 2.6 - t * 0.11));
    float curtain = smoothstep(0.32, 0.86, curtain_a * 0.72 + curtain_b * 0.5);
    float band = rise * (0.32 + 0.68 * curtain);

    // 透视半调点阵：越贴近地平线点越大、越亮。
    float spacing = 6. * max(glow.dpr, 0.5);
    float2 dot_uv = frac(local / spacing) - 0.5;
    float dot = saturate(1. - length(dot_uv) * 1.9);
    float proximity_to_horizon = saturate(1. - above * 1.4);
    float dots = smoothstep(0.45, 0.95, dot * (0.55 + 0.65 * proximity_to_horizon)) * glow.presence;

    float hue = frac(
        0.56 + 0.10 * glow.semantic + 0.12 * glow.spectrum * uv.y + 0.07 * curtain_a - t * 0.015
    );
    float3 aurora = hsv_to_rgb(float3(hue, lerp(0.42, 0.62, glow.spectrum), 0.78));

    float alpha = saturate((band * 0.9 + dots * 0.55) * glow.intensity);
    // 发消息那一瞬间整层亮一下（surge 由界面侧衰减）。
    alpha = saturate(alpha * (1. + glow.surge * 0.8));

    // 浅色底上往白里靠一档，免得在纸上糊成一片脏色；深色底保持饱和度。
    float3 color = lerp(aurora, float3(1., 1., 1.), 0.22 * (1. - glow.dark));
    color *= lerp(0.62, 1.08, saturate(band));

    if (alpha <= 0.002) {
        discard;
    }
    return float4(color * alpha, alpha);
}
