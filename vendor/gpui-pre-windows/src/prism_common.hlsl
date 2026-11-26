// claudesktop patch: 首页背景 / 输入框棱镜描边三个效果的公共头。
//
// 单独一份文件的原因见 `prism_rim.hlsl` 开头。这里只放和上游一致的常量缓冲、
// 坐标换算，以及三个效果都要用的噪声 / 配色工具。
//
// 约定：
// * 顶点着色器把 `bounds` 展开成 4 顶点三角带（和上游 quad 一样），
//   片元里的 `SV_Position.xy` 已经是**物理像素、左上角原点**（D3D 的口味）。
// * 所有片元一律输出预乘 alpha（`float4(rgb * a, a)`），管线用
//   `create_blend_state_for_path_sprite`（Src = ONE / Dst = INV_SRC_ALPHA）。
// * 结构体字段顺序与 `vendor/gpui-pre/src/scene.rs` 里的同名 Rust 结构体逐字节对应，
//   末尾的 `pad` 是撑到 16 字节整数倍用的，别删。

cbuffer GlobalParams: register(b0) {
    float4 gamma_ratios;
    float2 global_viewport_size;
    float grayscale_enhanced_contrast;
    float subpixel_enhanced_contrast;
    uint is_bgr;
    uint3 global_pad;
};

cbuffer BatchParams: register(b1) {
    uint batch_start_index;
    uint3 batch_pad;
};

struct Bounds {
    float2 origin;
    float2 size;
};

struct Corners {
    float top_left;
    float top_right;
    float bottom_right;
    float bottom_left;
};

float4 to_device_position(float2 position) {
    float2 device_position = position / global_viewport_size * float2(2.0, -2.0) + float2(-1.0, 1.0);
    return float4(device_position, 0., 1.);
}

float4 distance_from_clip_rect_impl(float2 position, Bounds clip_bounds) {
    float2 tl = position - clip_bounds.origin;
    float2 br = clip_bounds.origin + clip_bounds.size - position;
    return float4(tl.x, br.x, tl.y, br.y);
}

/// 三角带的四个单位顶点：位运算保证和上游 quad 完全一致。
float2 unit_vertex_for(uint vertex_id) {
    return float2(float(vertex_id & 1u), 0.5 * float(vertex_id & 2u));
}

// ---------------------------------------------------------------- 噪声

float hash21(float2 p) {
    p = frac(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return frac(p.x * p.y);
}

float value_noise(float2 p) {
    float2 i = floor(p);
    float2 f = frac(p);
    float2 u = f * f * (3. - 2. * f);
    float a = hash21(i);
    float b = hash21(i + float2(1., 0.));
    float c = hash21(i + float2(0., 1.));
    float d = hash21(i + float2(1., 1.));
    return lerp(lerp(a, b, u.x), lerp(c, d, u.x), u.y);
}

/// 4 个倍频的 fbm。5 个以上在 4_1 上意义不大，还会拖慢极光。
float fbm4(float2 p) {
    float sum = 0.;
    float amp = 0.5;
    [unroll]
    for (int i = 0; i < 4; i++) {
        sum += amp * value_noise(p);
        p = p * 2.03 + float2(17.3, 9.1);
        amp *= 0.5;
    }
    return sum;
}

// ---------------------------------------------------------------- 颜色

float3 hsv_to_rgb(float3 hsv) {
    float h = frac(hsv.x) * 6.;
    float s = saturate(hsv.y);
    float v = saturate(hsv.z);
    float c = v * s;
    float x = c * (1. - abs(fmod(h, 2.) - 1.));
    float3 rgb;
    if (h < 1.) {
        rgb = float3(c, x, 0.);
    } else if (h < 2.) {
        rgb = float3(x, c, 0.);
    } else if (h < 3.) {
        rgb = float3(0., c, x);
    } else if (h < 4.) {
        rgb = float3(0., x, c);
    } else if (h < 5.) {
        rgb = float3(x, 0., c);
    } else {
        rgb = float3(c, 0., x);
    }
    return rgb + (v - c);
}

/// 输入框描边的调色板：每个 tone 四个色标，按角度在环上取。
///
/// `dark` 只影响亮度和末端的提亮：深色底上同样的颜色会显得脏，得提一档。
/// 颜色是照着现有的暖纸 / 钴蓝 / 靛紫几套配色自己攒的（原始 JS 版本不暴露色值）。
float3 prism_palette(uint tone, float t, float dark) {
    float3 c0;
    float3 c1;
    float3 c2;
    float3 c3;
    if (tone == 1u) {
        // 暖纸：琥珀 → 珊瑚 → 梅 → 天青
        c0 = float3(0.98, 0.74, 0.42);
        c1 = float3(0.95, 0.48, 0.40);
        c2 = float3(0.74, 0.38, 0.62);
        c3 = float3(0.42, 0.60, 0.90);
    } else if (tone == 2u) {
        // 钴蓝
        c0 = float3(0.48, 0.74, 0.99);
        c1 = float3(0.36, 0.54, 0.96);
        c2 = float3(0.56, 0.44, 0.94);
        c3 = float3(0.30, 0.80, 0.88);
    } else if (tone == 3u) {
        // 靛紫
        c0 = float3(0.74, 0.64, 0.99);
        c1 = float3(0.62, 0.46, 0.96);
        c2 = float3(0.92, 0.54, 0.88);
        c3 = float3(0.52, 0.68, 0.97);
    } else if (tone == 4u) {
        // 琥珀
        c0 = float3(0.99, 0.78, 0.38);
        c1 = float3(0.96, 0.58, 0.30);
        c2 = float3(0.92, 0.42, 0.46);
        c3 = float3(0.80, 0.54, 0.88);
    } else if (tone == 5u) {
        // 纯灰：只有明暗，没有色相
        float g = dark > 0.5 ? 0.94 : 0.30;
        return float3(g, g, g);
    } else {
        // 默认：暖白
        c0 = float3(0.99, 0.86, 0.68);
        c1 = float3(0.97, 0.68, 0.64);
        c2 = float3(0.80, 0.66, 0.95);
        c3 = float3(0.64, 0.84, 0.96);
    }

    t = frac(t);
    float3 color;
    if (t < 0.3333) {
        color = lerp(c0, c1, t * 3.);
    } else if (t < 0.6666) {
        color = lerp(c1, c2, (t - 0.3333) * 3.);
    } else {
        color = lerp(c2, c3, (t - 0.6666) * 3.);
    }
    return dark > 0.5 ? saturate(color * 1.02 + 0.06) : color * 0.94;
}
