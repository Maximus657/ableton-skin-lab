// Skin Lab overlay, in two steps.
//
// ps_layer draws one picture of the skin into `stack` (premultiplied, the size of Live's window),
// cut to its panel by the scissor rectangle; the pictures go on top of each other in order. The
// stack only changes with the skin or the panels, not with every frame.
//
// ps_combine takes the captured Live window `ui`: wherever a pixel is (close to) the theme's
// background key colour, the stack shows there, carrying over the pixel's small difference from
// the key (grid lines, bar tiles, antialiased text edges) so those stay visible. Everything else is
// fully transparent, so the real Live window shows through. Right around Live's text the pictures
// give way to the plain panel colour, so the text stays readable. Output is premultiplied for
// DirectComposition, or (preview) opaque: what the screen will show.

Texture2D ui    : register(t0);
Texture2D stack : register(t1);
Texture2D pic   : register(t2);     // premultiplied, full mip chain
SamplerState samp : register(s0);   // trilinear, clamp: outside the picture its edges stretch

cbuffer Params : register(b0) {
    float4 key;          // key colour rgb (0..1)
    float4 misc;         // final step: how much the pictures give way around Live's text (0..1);
                         // 1 = a whole-window background (fades by the panels' backdrop);
                         // final step: 1 = preview, frame pixels per output pixel
    float4 place;        // where the picture is drawn: x, y, w, h in frame pixels
    float4 look;         // brightness (ink: strength), blur px, opacity, ink mode (0/1)
    float4 ink;          // ink colour rgb, soft edge (share of the picture)
    float4 picinfo;      // picture width, height (texels), mip levels, edge smear px (0: nothing around it)
    float4 flags;        // mirrored (0/1), paper lightness (line art), quarter turns clockwise,
                         // black takes the panel colour `key` (0/1)
    float4 panels[6];    // panel rectangles (x, y, w, h), for the backdrop
    float4 backdrop[6];  // .x: how much of the whole-window backgrounds shows in that panel
};

struct VSOut { float4 pos : SV_Position; };

static const float3 LUMA = float3(0.2126, 0.7152, 0.0722);

VSOut vs_main(uint id : SV_VertexID) {
    VSOut o;
    float2 t = float2((id << 1) & 2, id & 2);
    o.pos = float4(t * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

static const float2 RING[8] = {
    float2(1, 0), float2(-1, 0), float2(0, 1), float2(0, -1),
    float2(0.7071, 0.7071), float2(-0.7071, 0.7071), float2(0.7071, -0.7071), float2(-0.7071, -0.7071)
};

// A mip level is a box blur of 2^lod texels; eight taps around it hide the blockiness.
float4 soft(float2 uv, float lod, float2 texel) {
    float4 c = pic.SampleLevel(samp, uv, lod) * 0.2;
    float2 r = texel * exp2(lod);
    [unroll] for (int i = 0; i < 8; i++)
        c += pic.SampleLevel(samp, uv + RING[i] * r, lod) * 0.1;
    return c;
}

// where a point of the drawn (turned) picture is in the texture
float2 turn(float2 d) {
    int r = (int)flags.z;
    if (r == 1) return float2(d.y, 1 - d.x);
    if (r == 2) return 1 - d;
    if (r == 3) return float2(1 - d.y, d.x);
    return d;
}

float4 ps_layer(VSOut i) : SV_Target {
    float2 p = i.pos.xy;
    float2 uv = (p - place.xy) / place.zw;
    if (flags.x > 0.5)
        uv.x = 1 - uv.x;
    float texPerPx = ((int)flags.z % 2 == 1 ? picinfo.y : picinfo.x) / place.z;
    float2 texel = 1.0 / picinfo.xy;
    float maxLod = picinfo.z - 1;
    float baseLod = clamp(log2(max(texPerPx, 1e-4)), 0, maxLod);

    float4 inside;
    if (look.y >= 0.5)
        inside = soft(turn(uv), clamp(log2(look.y * texPerPx), baseLod, maxLod), texel);
    else
        inside = pic.SampleLevel(samp, turn(uv), baseLod);

    float2 f2 = saturate(min(uv, 1 - uv) / max(ink.w, 1e-3));
    float f = (all(uv >= 0) && all(uv <= 1)) ? f2.x * f2.y : 0;
    float4 c;
    if (picinfo.w > 0) {
        // the backdrop around a picture that does not fill its panel: its own edges, smeared
        float edgeLod = clamp(log2(picinfo.w * texPerPx), baseLod, maxLod);
        c = lerp(soft(turn(saturate(uv)), edgeLod, texel), inside, f);
    } else {
        c = inside * f;
    }

    float a = c.a;
    float3 rgb = c.rgb / max(a, 1e-4);
    if (look.w > 0.5) {
        // line art: the paper goes transparent, the lines take the ink colour
        float lum = dot(rgb, LUMA);
        a *= saturate((1 - lum / flags.y) * look.x);
        rgb = ink.rgb;
    } else {
        rgb *= look.x;
        if (flags.w > 0.5) {
            // screen blending over the panel colour: a black picture background melts into the
            // panel instead of showing as a dark patch; only what is lighter than it shows
            rgb = rgb + key.rgb - rgb * key.rgb;
        }
    }
    a *= look.z;
    if (misc.y > 0.5) {
        bool hit = false;
        [unroll] for (int k = 0; k < 6; k++) {
            if (!hit && all(p >= panels[k].xy) && all(p < panels[k].xy + panels[k].zw)) {
                a *= backdrop[k].x;
                hit = true;
            }
        }
    }
    return float4(saturate(rgb) * a, a);
}

// How much a frame pixel that differs from the key by d is still the panel background.
// Live darkens the panel by up to a tenth in places of its own (every other bar in the
// arrangement and the clip editor, soft shadows): that is still the panel, so the pictures show
// there in full and just as much darker; if they showed less there, they would come out striped.
// Anything lighter than the panel (text edges, the selected track's lane) counts only when very
// close to it, so a picture never shows half through it.
float cover(float3 d) {
    float dist = max(abs(d.r), max(abs(d.g), abs(d.b)));
    bool darker = all(d <= 1.5 / 255);
    float on = darker ? 0.1 * max(key.r, max(key.g, key.b)) + 1.5 / 255 : 2.0 / 255;
    float off = darker ? on * 2 : 6.0 / 255;
    return saturate((off - dist) / (off - on));
}

static const int2 AROUND[16] = {
    int2(1, 0), int2(-1, 0), int2(0, 1), int2(0, -1),
    int2(1, 1), int2(-1, 1), int2(1, -1), int2(-1, -1),
    int2(2, 0), int2(-2, 0), int2(0, 2), int2(0, -2),
    int2(2, 2), int2(-2, 2), int2(2, -2), int2(-2, -2)
};

// How close pixel q is to something Live draws much lighter than the panel: text, icons, clips
// (grid lines and other dim marks do not count).
float near_text(int2 q) {
    float kl = dot(key.rgb, LUMA);
    float n = 0;
    [unroll] for (int k = 0; k < 16; k++) {
        float l = dot(ui.Load(int3(q + AROUND[k], 0)).rgb, LUMA);
        n = max(n, smoothstep(kl + 0.15, kl + 0.35, l) * (k < 8 ? 1.0 : 0.7));
    }
    return n;
}

float4 overlay(int2 q, float3 u) {
    float4 s = stack.Load(int3(q, 0));
    float3 d = u - key.rgb;
    float a = s.a * cover(d);
    if (a <= 0)
        return float4(0, 0, 0, 0);
    float3 c = s.rgb / s.a;
    if (misc.x > 0) {
        // right around the text the picture gives way to the plain panel colour (where it is
        // lighter than that), so light text stays readable on a light picture
        c = lerp(c, min(c, key.rgb), near_text(q) * misc.x);
    }
    return float4(saturate(c + d) * a, a);
}

float4 ps_combine(VSOut i) : SV_Target {
    if (misc.z < 0.5) {
        int2 q = int2(i.pos.xy);
        return overlay(q, ui.Load(int3(q, 0)).rgb);
    }
    // preview, possibly smaller than the frame: the mean of the frame pixels whose centres fall
    // in this output pixel (a few point samples would drop thin lines and show moire)
    int2 lo = int2(ceil(floor(i.pos.xy) * misc.w - 0.5));
    int2 hi = max(int2(ceil((floor(i.pos.xy) + 1) * misc.w - 0.5)), lo + 1);
    float3 acc = 0;
    [loop] for (int y = lo.y; y < hi.y; y++) {
        [loop] for (int x = lo.x; x < hi.x; x++) {
            int2 q = int2(x, y);
            float3 u = ui.Load(int3(q, 0)).rgb;
            float4 o = overlay(q, u);
            acc += o.rgb + u * (1 - o.a);
        }
    }
    return float4(acc / ((hi.x - lo.x) * (hi.y - lo.y)), 1);
}
