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

float4 lerp_rim_stops5(
    float r,
    float4 c0,
    float p1,
    float4 c1,
    float p2,
    float4 c2,
    float p3,
    float4 c3,
    float4 c4
) {
    if (r < p1) {
        return lerp(c0, c1, r / max(p1, 1e-4));
    }
    if (r < p2) {
        return lerp(c1, c2, (r - p1) / max(p2 - p1, 1e-4));
    }
    if (r < p3) {
        return lerp(c2, c3, (r - p2) / max(p3 - p2, 1e-4));
    }
    return lerp(c3, c4, (r - p3) / max(1. - p3, 1e-4));
}

/// 输入框描边的径向多色标宝石光谱：1:1 对照 obsidian-prism 的 `RIM_PALETTES`。
///
/// 按指针归一化半径 `r = dist / radius`（`0..1`）采样，返回 `float4(rgb, stop_alpha)`，
/// 到达外沿 `r >= 1.0` 时 alpha 归零。
float4 prism_rim_sample(uint tone, float r, float dark) {
    r = saturate(r);
    if (r >= 1.) {
        return float4(0., 0., 0., 0.);
    }
    bool is_dark = dark > 0.5;
    if (tone == 1u) {
        if (is_dark) {
            return lerp_rim_stops5(
                r,
                float4(1.0, 0.965, 0.933, 0.95),
                0.26, float4(0.965, 0.741, 0.376, 1.0),
                0.54, float4(0.941, 0.639, 0.494, 1.0),
                0.82, float4(0.851, 0.467, 0.341, 1.0),
                float4(0.851, 0.467, 0.341, 0.0)
            );
        }
        return lerp_rim_stops5(
            r,
            float4(0.604, 0.204, 0.071, 1.0),
            0.34, float4(0.761, 0.349, 0.204, 1.0),
            0.68, float4(0.851, 0.467, 0.341, 1.0),
            0.90, float4(0.851, 0.467, 0.024, 1.0),
            float4(0.851, 0.467, 0.024, 0.0)
        );
    }
    if (tone == 2u) {
        if (is_dark) {
            return lerp_rim_stops5(
                r,
                float4(0.941, 0.976, 1.0, 0.96),
                0.26, float4(0.486, 0.886, 1.0, 1.0),
                0.56, float4(0.259, 0.522, 0.957, 1.0),
                0.86, float4(0.106, 0.286, 0.698, 1.0),
                float4(0.106, 0.286, 0.698, 0.0)
            );
        }
        return lerp_rim_stops5(
            r,
            float4(0.114, 0.306, 0.847, 1.0),
            0.35, float4(0.145, 0.388, 0.922, 1.0),
            0.70, float4(0.008, 0.518, 0.780, 1.0),
            0.92, float4(0.220, 0.741, 0.973, 1.0),
            float4(0.220, 0.741, 0.973, 0.0)
        );
    }
    if (tone == 3u) {
        if (is_dark) {
            return lerp_rim_stops5(
                r,
                float4(0.961, 0.953, 1.0, 0.95),
                0.26, float4(0.867, 0.839, 0.996, 1.0),
                0.56, float4(0.647, 0.541, 0.973, 1.0),
                0.86, float4(0.427, 0.157, 0.851, 1.0),
                float4(0.427, 0.157, 0.851, 0.0)
            );
        }
        return lerp_rim_stops5(
            r,
            float4(0.357, 0.129, 0.714, 1.0),
            0.36, float4(0.486, 0.227, 0.929, 1.0),
            0.70, float4(0.545, 0.361, 0.965, 1.0),
            0.92, float4(0.659, 0.333, 0.969, 1.0),
            float4(0.659, 0.333, 0.969, 0.0)
        );
    }
    if (tone == 4u) {
        if (is_dark) {
            return lerp_rim_stops5(
                r,
                float4(1.0, 0.984, 0.922, 0.95),
                0.26, float4(0.992, 0.902, 0.541, 1.0),
                0.56, float4(0.949, 0.722, 0.294, 1.0),
                0.86, float4(0.851, 0.467, 0.024, 1.0),
                float4(0.851, 0.467, 0.024, 0.0)
            );
        }
        return lerp_rim_stops5(
            r,
            float4(0.573, 0.251, 0.055, 1.0),
            0.36, float4(0.706, 0.325, 0.035, 1.0),
            0.70, float4(0.851, 0.467, 0.024, 1.0),
            0.92, float4(0.961, 0.620, 0.043, 1.0),
            float4(0.961, 0.620, 0.043, 0.0)
        );
    }
    if (tone == 5u) {
        if (is_dark) {
            return lerp_rim_stops5(
                r,
                float4(0.973, 0.980, 0.988, 0.92),
                0.28, float4(0.886, 0.910, 0.941, 1.0),
                0.60, float4(0.580, 0.639, 0.722, 1.0),
                0.88, float4(0.392, 0.455, 0.545, 1.0),
                float4(0.392, 0.455, 0.545, 0.0)
            );
        }
        return lerp_rim_stops5(
            r,
            float4(0.118, 0.161, 0.231, 1.0),
            0.36, float4(0.200, 0.255, 0.333, 1.0),
            0.70, float4(0.392, 0.455, 0.545, 1.0),
            0.92, float4(0.580, 0.639, 0.722, 1.0),
            float4(0.580, 0.639, 0.722, 0.0)
        );
    }
    if (is_dark) {
        if (r < 0.92) {
            return lerp_rim_stops5(
                r / 0.92,
                float4(1.0, 1.0, 1.0, 0.96),
                0.24 / 0.92, float4(0.259, 0.522, 0.957, 1.0),
                0.50 / 0.92, float4(0.608, 0.447, 0.796, 1.0),
                0.74 / 0.92, float4(0.918, 0.263, 0.208, 1.0),
                float4(0.984, 0.737, 0.016, 1.0)
            );
        }
        return lerp(
            float4(0.984, 0.737, 0.016, 1.0),
            float4(0.984, 0.737, 0.016, 0.0),
            (r - 0.92) / 0.08
        );
    }
    return lerp_rim_stops5(
        r,
        float4(0.145, 0.388, 0.922, 1.0),
        0.32, float4(0.486, 0.227, 0.929, 1.0),
        0.64, float4(0.918, 0.263, 0.208, 1.0),
        0.88, float4(0.851, 0.467, 0.024, 1.0),
        float4(0.851, 0.467, 0.024, 0.0)
    );
}
