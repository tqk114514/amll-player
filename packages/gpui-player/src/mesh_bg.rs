//! mesh 渐变背景 CPU 侧(上游 `bg-render/mesh-renderer` 的移植)。
//!
//! 本模块的产物(MeshBackgroundResource)经渲染器 patch
//! (paint_mesh_background)绘制。
//!
//! 上游机制:专辑图 32×32 化(对比 0.4 → 饱和 3.0 → 对比 1.7 →
//! 亮度 0.75 → StackBlur radius=2 quality=4)作为纹理;Hermite 双三次
//! patch 网格按控制点预设(或 20% 概率随机生成)细分 50 级,顶点
//! (pos+color+uv)一次性烘顶;每帧仅 shader 侧用 u_time/u_volume
//! (低频/10)驱动采样旋转缩放。GPU 管线见渲染器 patch。
//!
//! 细分数学严格按上游 JS 移植:gl-matrix 列主元布局、同序运算、
//! f64 精度(JS number 即 f64),保证顶点逐位一致。

use std::sync::Arc;

/// 上游 Mesh 顶点布局:pos(2) + color(3) + uv(2)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshVertex {
    pub pos: [f32; 2],
    pub color: [f32; 3],
    pub uv: [f32; 2],
}

/// 一份可绘制的 mesh 背景资源(纹理 + 网格),id 供渲染器缓存失效
#[derive(Debug, Clone)]
pub struct MeshBackgroundResource {
    pub id: u64,
    pub vertices: Arc<Vec<MeshVertex>>,
    pub cells_across: u32,
    pub cell_count: u32,
    pub texture_width: u32,
    pub texture_height: u32,
    pub texture: Arc<Vec<u8>>,
}

/// 上游细分级别(BHPMesh.resetSubdivition(50))
const SUBDIVISIONS: usize = 50;
/// 上游 reduceImageSizeCanvas 32×32
const ALBUM_SIZE: u32 = 32;

// ---------------------------------------------------------------------------
// FLAC 封面提取(fLaC 原生 PICTURE 块,类型 6)
// ---------------------------------------------------------------------------

/// 从 FLAC 文件提取内嵌封面(原生 PICTURE 块;无则 None)
pub fn extract_flac_cover(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 8 || &bytes[0..4] != b"fLaC" {
        return None;
    }
    let mut offset = 4usize;
    loop {
        if offset + 4 > bytes.len() {
            return None;
        }
        let header = bytes[offset];
        let block_type = header & 0x7f;
        let is_last = header & 0x80 != 0;
        let length = ((bytes[offset + 1] as usize) << 16)
            | ((bytes[offset + 2] as usize) << 8)
            | bytes[offset + 3] as usize;
        offset += 4;
        if block_type == 6 && offset + length <= bytes.len() {
            // PICTURE: type(4) mimeLen(4) mime descLen(4) desc
            //          w(4) h(4) depth(4) colors(4) dataLen(4) data
            let block_end = offset + length;
            let fits = |p: usize, n: usize| p + n <= block_end;
            let mut p = offset;
            if !fits(p, 4 + 4) {
                return None;
            }
            p += 4; // 图片类型
            let mime_len = u32::from_be_bytes(bytes[p..p + 4].try_into().ok()?) as usize;
            p += 4;
            if !fits(p, 4 + mime_len + 4) {
                return None;
            }
            p += mime_len;
            let desc_len = u32::from_be_bytes(bytes[p..p + 4].try_into().ok()?) as usize;
            p += 4;
            if !fits(p, desc_len + 20) {
                return None;
            }
            p += desc_len;
            p += 16; // 宽、高、位深、颜色数
            if !fits(p, 4) {
                return None;
            }
            let data_len = u32::from_be_bytes(bytes[p..p + 4].try_into().ok()?) as usize;
            p += 4;
            if p + data_len <= block_end {
                return Some(bytes[p..p + data_len].to_vec());
            }
            return None;
        }
        offset += length;
        if is_last || offset >= bytes.len() {
            return None;
        }
    }
}

// ---------------------------------------------------------------------------
// 专辑图处理(上游 setAlbum 的像素管线)
// ---------------------------------------------------------------------------

/// 上游像素管线:对比 0.4 → 饱和 3.0 → 对比 1.7 → 亮度 0.75 →
/// StackBlur(radius=2, quality=4)。输入任意尺寸图,输出 32×32 RGBA8。
pub fn process_album(image: image::RgbaImage) -> image::RgbaImage {
    let resized =
        image::imageops::resize(&image, ALBUM_SIZE, ALBUM_SIZE, image::imageops::FilterType::Nearest);
    let mut out = resized.into_raw();
    // contrast 0.4 → saturate 3.0 → contrast 1.7 → brightness 0.75
    // (上游 JS f64 中间值不截断,统一先算满四步再回写)
    for i in 0..out.len() / 4 {
        let chunk = &mut out[i * 4..i * 4 + 4];
        let [r, g, b, _] = chunk else { continue };
        let rgb = [
            (*r as f64 - 128.0) * 0.4 + 128.0,
            (*g as f64 - 128.0) * 0.4 + 128.0,
            (*b as f64 - 128.0) * 0.4 + 128.0,
        ];
        let gray = rgb[0] * 0.3 + rgb[1] * 0.59 + rgb[2] * 0.11;
        let out_rgb = [
            (gray * -2.0 + rgb[0] * 3.0 - 128.0) * 1.7 + 128.0,
            (gray * -2.0 + rgb[1] * 3.0 - 128.0) * 1.7 + 128.0,
            (gray * -2.0 + rgb[2] * 3.0 - 128.0) * 1.7 + 128.0,
        ];
        *r = (out_rgb[0] * 0.75).round().clamp(0.0, 255.0) as u8;
        *g = (out_rgb[1] * 0.75).round().clamp(0.0, 255.0) as u8;
        *b = (out_rgb[2] * 0.75).round().clamp(0.0, 255.0) as u8;
    }
    stack_blur(&mut out, ALBUM_SIZE as usize, ALBUM_SIZE as usize, 2, 4);
    image::RgbaImage::from_raw(ALBUM_SIZE, ALBUM_SIZE, out)
        .expect("尺寸不变")
}

/// 上游 blurImage(StackBlur,水平/垂直滑动窗口各 quality 遍)
#[allow(clippy::needless_range_loop)]
pub fn stack_blur(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    radius: usize,
    quality: usize,
) {
    let wm = width - 1;
    let hm = height - 1;
    let rad1x = radius + 1;
    let divx = radius + rad1x;
    let rad1y = radius + 1;
    let divy = radius + rad1y;
    let div2 = 1.0 / (divx * divy) as f64;

    let mut r = vec![0i64; width * height];
    let mut g = vec![0i64; width * height];
    let mut b = vec![0i64; width * height];
    let mut a = vec![0i64; width * height];
    let mut vmin = vec![0usize; width.max(height)];
    let mut vmax = vec![0usize; width.max(height)];

    for _ in 0..quality {
        let mut yw = 0usize;
        let mut yi = 0usize;
        for y in 0..height {
            let mut rsum = pixels[yw] as i64 * rad1x as i64;
            let mut gsum = pixels[yw + 1] as i64 * rad1x as i64;
            let mut bsum = pixels[yw + 2] as i64 * rad1x as i64;
            let mut asum = pixels[yw + 3] as i64 * rad1x as i64;
            for i in 1..=radius {
                let p = yw + (if i > wm { wm } else { i } << 2);
                rsum += pixels[p] as i64;
                gsum += pixels[p + 1] as i64;
                bsum += pixels[p + 2] as i64;
                asum += pixels[p + 3] as i64;
            }
            for x in 0..width {
                r[yi] = rsum;
                g[yi] = gsum;
                b[yi] = bsum;
                a[yi] = asum;
                if y == 0 {
                    vmin[x] = (x + rad1x).min(wm) << 2;
                    vmax[x] = x.saturating_sub(radius) << 2;
                }
                let p1 = yw + vmin[x];
                let p2 = yw + vmax[x];
                rsum += pixels[p1] as i64 - pixels[p2] as i64;
                gsum += pixels[p1 + 1] as i64 - pixels[p2 + 1] as i64;
                bsum += pixels[p1 + 2] as i64 - pixels[p2 + 2] as i64;
                asum += pixels[p1 + 3] as i64 - pixels[p2 + 3] as i64;
                yi += 1;
            }
            yw += width << 2;
        }
        for x in 0..width {
            let mut yp = x;
            let mut rsum = r[yp] * rad1y as i64;
            let mut gsum = g[yp] * rad1y as i64;
            let mut bsum = b[yp] * rad1y as i64;
            let mut asum = a[yp] * rad1y as i64;
            for i in 1..=radius {
                yp += if i > hm { 0 } else { width };
                rsum += r[yp];
                gsum += g[yp];
                bsum += b[yp];
                asum += a[yp];
            }
            let mut yi = x << 2;
            for y in 0..height {
                pixels[yi] = (rsum as f64 * div2 + 0.5) as u8;
                pixels[yi + 1] = (gsum as f64 * div2 + 0.5) as u8;
                pixels[yi + 2] = (bsum as f64 * div2 + 0.5) as u8;
                pixels[yi + 3] = (asum as f64 * div2 + 0.5) as u8;
                if x == 0 {
                    vmin[y] = (y + rad1y).min(hm) * width;
                    vmax[y] = y.saturating_sub(radius) * width;
                }
                let p1 = x + vmin[y];
                let p2 = x + vmax[y];
                rsum += r[p1] - r[p2];
                gsum += g[p1] - g[p2];
                bsum += b[p1] - b[p2];
                asum += a[p1] - a[p2];
                yi += width << 2;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 控制点(上游 cp-presets + cp-generate)
// ---------------------------------------------------------------------------

/// 上游 ControlPointConf
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlPointConf {
    pub cx: usize,
    pub cy: usize,
    pub x: f64,
    pub y: f64,
    pub ur: f64,
    pub vr: f64,
    pub up: f64,
    pub vp: f64,
}

fn cp(cx: usize, cy: usize, x: f64, y: f64) -> ControlPointConf {
    ControlPointConf { cx, cy, x, y, ur: 0.0, vr: 0.0, up: 1.0, vp: 1.0 }
}

#[allow(clippy::too_many_arguments)]
fn cp_full(cx: usize, cy: usize, x: f64, y: f64, ur: f64, vr: f64, up: f64, vp: f64) -> ControlPointConf {
    ControlPointConf { cx, cy, x, y, ur, vr, up, vp }
}

/// 简单确定性 RNG(xorshift64*),种子取专辑数据哈希——上游 Math.random
/// 每专辑重摇,同专辑稳定
#[derive(Debug, Clone)]
pub struct MeshRng(u64);

impl MeshRng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_f64(&mut self) -> f64 {
        // xorshift64*:2^64 网格取 [0,1)
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let v = x.wrapping_mul(0x2545F4914F6CDD1D);
        (v >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, min: f64, max: f64) -> f64 {
        min + self.next_f64() * (max - min)
    }
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 上游 smoothifyControlPoints:3×3 加权核平滑非边界控制点
fn smoothify_control_points(conf: &mut [ControlPointConf], w: usize, h: usize) {
    const KERNEL: [[f64; 3]; 3] = [[1.0, 2.0, 1.0], [2.0, 4.0, 2.0], [1.0, 2.0, 1.0]];
    let kernel_sum = 16.0;
    let smooth_iters = self::smooth_iters();
    let factor = 0.25f64;
    let factor_iteration_modifier = 0.1f64;

    let mut grid: Vec<Vec<ControlPointConf>> = (0..h)
        .map(|j| (0..w).map(|i| conf[j * w + i]).collect())
        .collect();
    let mut f = factor;
    for _ in 0..smooth_iters {
        let mut new_grid = vec![vec![cp(0, 0, 0.0, 0.0); w]; h];
        for j in 0..h {
            for i in 0..w {
                if i == 0 || i == w - 1 || j == 0 || j == h - 1 {
                    new_grid[j][i] = grid[j][i];
                    continue;
                }
                let (mut sx, mut sy, mut sur, mut svr, mut sup, mut svp) =
                    (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
                for dj in -1i64..=1 {
                    for di in -1i64..=1 {
                        let weight = KERNEL[(dj + 1) as usize][(di + 1) as usize];
                        let nb = &grid[(j as i64 + dj) as usize][(i as i64 + di) as usize];
                        sx += nb.x * weight;
                        sy += nb.y * weight;
                        sur += nb.ur * weight;
                        svr += nb.vr * weight;
                        sup += nb.up * weight;
                        svp += nb.vp * weight;
                    }
                }
                let cur = &grid[j][i];
                new_grid[j][i] = cp_full(
                    i,
                    j,
                    cur.x * (1.0 - f) + sx / kernel_sum * f,
                    cur.y * (1.0 - f) + sy / kernel_sum * f,
                    cur.ur * (1.0 - f) + sur / kernel_sum * f,
                    cur.vr * (1.0 - f) + svr / kernel_sum * f,
                    cur.up * (1.0 - f) + sup / kernel_sum * f,
                    cur.vp * (1.0 - f) + svp / kernel_sum * f,
                );
            }
        }
        grid = new_grid;
        f = (f + factor_iteration_modifier).clamp(0.0, 1.0);
    }
    for j in 0..h {
        for i in 0..w {
            conf[j * w + i] = grid[j][i];
        }
    }
}

/// 上游 smoothIters = floor(randomRange(3,5))(以种子 0 稳定为 4)
fn smooth_iters() -> usize {
    4
}

fn fract(x: f64) -> f64 {
    x - x.floor()
}

fn noise_hash(x: f64, y: f64) -> f64 {
    fract((x * 12.9898 + y * 78.233).sin() * 43758.5453)
}

fn smooth_noise(x: f64, y: f64) -> f64 {
    let x0 = x.floor();
    let y0 = y.floor();
    let xf = x - x0;
    let yf = y - y0;
    let u = xf * xf * (3.0 - 2.0 * xf);
    let v = yf * yf * (3.0 - 2.0 * yf);
    let n00 = noise_hash(x0, y0);
    let n10 = noise_hash(x0 + 1.0, y0);
    let n01 = noise_hash(x0, y0 + 1.0);
    let n11 = noise_hash(x0 + 1.0, y0 + 1.0);
    let nx0 = n00 * (1.0 - u) + n10 * u;
    let nx1 = n01 * (1.0 - u) + n11 * u;
    nx0 * (1.0 - v) + nx1 * v
}

fn noise_gradient(x: f64, y: f64, epsilon: f64) -> (f64, f64) {
    let dx = (smooth_noise(x + epsilon, y) - smooth_noise(x - epsilon, y)) / (2.0 * epsilon);
    let dy = (smooth_noise(x, y + epsilon) - smooth_noise(x, y - epsilon)) / (2.0 * epsilon);
    let len = (dx * dx + dy * dy).sqrt().max(1e-12);
    (dx / len, dy / len)
}

/// 上游 generateControlPoints(w, h)(默认参数的确定化端口:
/// variation 0.5 / normalOffset 0.45 / blend 0.8 / smoothFactor 0.25 /
/// smoothModifier −0.075,其余由 rng 摇)
pub fn generate_control_points(width: usize, height: usize, rng: &mut MeshRng) -> Vec<ControlPointConf> {
    let w = width;
    let h = height;
    let variation_fraction = rng.range(0.4, 0.6);
    let normal_offset = rng.range(0.3, 0.6);
    let blend_factor = 0.8;
    let smooth_factor = rng.range(0.2, 0.3);

    let mut conf = Vec::with_capacity(w * h);
    let dx = if w == 1 { 0.0 } else { 2.0 / (w - 1) as f64 };
    let dy = if h == 1 { 0.0 } else { 2.0 / (h - 1) as f64 };
    for j in 0..h {
        for i in 0..w {
            let base_x = (if w == 1 { 0.0 } else { i as f64 / (w - 1) as f64 }) * 2.0 - 1.0;
            let base_y = (if h == 1 { 0.0 } else { j as f64 / (h - 1) as f64 }) * 2.0 - 1.0;
            let is_border = i == 0 || i == w - 1 || j == 0 || j == h - 1;
            let (pert_x, pert_y) = if is_border {
                (0.0, 0.0)
            } else {
                (
                    rng.range(-variation_fraction * dx, variation_fraction * dx),
                    rng.range(-variation_fraction * dy, variation_fraction * dy),
                )
            };
            let mut x = base_x + pert_x;
            let mut y = base_y + pert_y;
            let (ur, vr, up, vp) = if is_border {
                (0.0, 0.0, 1.0, 1.0)
            } else {
                (rng.range(-60.0, 60.0), rng.range(-60.0, 60.0), rng.range(0.8, 1.2), rng.range(0.8, 1.2))
            };
            if !is_border {
                let u_norm = (base_x + 1.0) / 2.0;
                let v_norm = (base_y + 1.0) / 2.0;
                let (nx, ny) = noise_gradient(u_norm, v_norm, 0.001);
                let mut offset_x = nx * normal_offset;
                let mut offset_y = ny * normal_offset;
                let dist_to_border = u_norm.min(1.0 - u_norm).min(v_norm.min(1.0 - v_norm));
                let weight = smoothstep(0.0, 1.0, dist_to_border);
                offset_x *= weight;
                offset_y *= weight;
                x = x * (1.0 - blend_factor) + (x + offset_x) * blend_factor;
                y = y * (1.0 - blend_factor) + (y + offset_y) * blend_factor;
            }
            conf.push(cp_full(i, j, x, y, ur, vr, up, vp));
        }
    }
    let _ = smooth_factor;
    smoothify_control_points(&mut conf, w, h);
    conf
}

/// 上游 CONTROL_POINT_PRESETS(六组,顺序一致)
// 预设数值逐位取自上游 cp-presets.ts(其中 -0.7853 巧合近似
// -FRAC_PI_4,属原始数据而非数学常量),故豁免 approx_constant
#[allow(clippy::approx_constant)]
#[allow(clippy::vec_init_then_push)]
pub fn control_point_presets() -> Vec<(usize, usize, Vec<ControlPointConf>)> {
    let mut out = Vec::new();
    // 1: 5×5
    out.push((
        5,
        5,
        vec![
            cp(0, 0, -1.0, -1.0), cp(1, 0, -0.5, -1.0), cp(2, 0, 0.0, -1.0), cp(3, 0, 0.5, -1.0), cp(4, 0, 1.0, -1.0),
            cp(0, 1, -1.0, -0.5), cp(1, 1, -0.5, -0.5),
            cp(2, 1, -0.0052029684413368305, -0.6131420587090777),
            cp(3, 1, 0.5884227308309977, -0.3990805107556692), cp(4, 1, 1.0, -0.5),
            cp(0, 2, -1.0, 0.0),
            cp(1, 2, -0.4210024670505933, -0.11895058380429502),
            cp_full(2, 2, -0.1019613423315412, -0.023812118047224606, 0.0, -47.0, 0.629, 0.849),
            cp(3, 2, 0.40275125660925437, -0.06345314544600389), cp(4, 2, 1.0, 0.0),
            cp(0, 3, -1.0, 0.5),
            cp_full(1, 3, 0.06801958477287173, 0.5205913248960121, -31.0, -45.0, 1.0, 1.0),
            cp_full(2, 3, 0.21446469120128908, 0.29331610114301043, 6.0, -56.0, 0.566, 1.321),
            cp(3, 3, 0.5, 0.5), cp(4, 3, 1.0, 0.5),
            cp(0, 4, -1.0, 1.0),
            cp(1, 4, -0.31378372841550195, 1.0), cp(2, 4, 0.26153633255328046, 1.0),
            cp(3, 4, 0.5, 1.0), cp(4, 4, 1.0, 1.0),
        ],
    ));
    // 2: 4×4 横屏
    out.push((
        4,
        4,
        vec![
            cp(0, 0, -1.0, -1.0), cp(1, 0, -0.33333333333333337, -1.0),
            cp(2, 0, 0.33333333333333326, -1.0), cp(3, 0, 1.0, -1.0),
            cp(0, 1, -1.0, -0.04495399932657351),
            cp(1, 1, -0.24056117520129328, -0.22465999020104), cp(2, 1, 0.334758885767489, -0.00531297192779423),
            cp_full(3, 1, 0.9989920470678106, -0.3382976020775408, 8.0, 0.0, 0.566, 1.792),
            cp(0, 2, -1.0, 0.33333333333333326),
            cp(1, 2, -0.3425497314639411, -0.000027501607956947893),
            cp(2, 2, 0.3321437945812673, 0.1981776353859399), cp(3, 2, 1.0, 0.0766118180296832),
            cp(0, 3, -1.0, 1.0), cp(1, 3, -0.33333333333333337, 1.0),
            cp(2, 3, 0.33333333333333326, 1.0), cp(3, 3, 1.0, 1.0),
        ],
    ));
    // 3: 4×4
    out.push((
        4,
        4,
        vec![
            cp_full(0, 0, -1.0, -1.0, 0.0, 0.0, 1.0, 2.075), cp(1, 0, -0.33333333333333337, -1.0),
            cp(2, 0, 0.33333333333333326, -1.0), cp(3, 0, 1.0, -1.0),
            cp(0, 1, -1.0, -0.4545779491139603), cp(1, 1, -0.33333333333333337, -0.33333333333333337),
            cp_full(2, 1, 0.0889403142626457, -0.6025711180694033, -32.0, 45.0, 1.0, 1.0),
            cp(3, 1, 1.0, -0.33333333333333337),
            cp_full(0, 2, -1.0, -0.07402408608567845, 1.0, 0.0, 1.0, 0.094),
            cp_full(1, 2, -0.2719422694359541, 0.09775369930903222, 25.0, -18.0, 1.321, 0.0),
            cp_full(2, 2, 0.19877414408395877, 0.4307383294587789, 48.0, -40.0, 0.755, 0.975),
            cp_full(3, 2, 1.0, 0.33333333333333326, -37.0, 0.0, 1.0, 1.0),
            cp(0, 3, -1.0, 1.0), cp(1, 3, -0.33333333333333337, 1.0),
            cp_full(2, 3, 0.5125850864305672, 1.0, -20.0, -18.0, 0.0, 1.604), cp(3, 3, 1.0, 1.0),
        ],
    ));
    // 4: 5×5
    out.push((
        5,
        5,
        vec![
            cp(0, 0, -1.0, -1.0), cp_full(1, 0, -0.4501953125, -1.0, 0.0, 55.0, 1.0, 2.075),
            cp(2, 0, 0.1953125, -1.0), cp_full(3, 0, 0.4580078125, -1.0, 0.0, -25.0, 1.0, 1.0),
            cp(4, 0, 1.0, -1.0),
            cp_full(0, 1, -1.0, -0.2514475377525607, -16.0, 0.0, 2.327, 0.943),
            cp_full(1, 1, -0.55859375, -0.6609325945787148, 47.0, 0.0, 2.358, 0.377),
            cp_full(2, 1, 0.232421875, -0.5244375756366635, -66.0, -25.0, 1.855, 1.164),
            cp(3, 1, 0.685546875, -0.3753706470552125), cp(4, 1, 1.0, -0.6699125300354287),
            cp(0, 2, -1.0, 0.035910396862284255),
            cp_full(1, 2, -0.4921875, 0.005378616309457018, 90.0, 23.0, 1.0, 1.981),
            cp_full(2, 2, 0.021484375, -0.1365043638066228, 0.0, 42.0, 1.0, 1.0),
            cp_full(3, 2, 0.4765625, 0.05925822904974043, -30.0, 0.0, 1.95, 0.44),
            cp(4, 2, 1.0, 0.251428847823418),
            cp_full(0, 3, -1.0, 0.6968336464764276, -68.0, 0.0, 1.0, 0.786),
            cp_full(1, 3, -0.6904296875, 0.5890744209958608, -68.0, 0.0, 1.0, 1.0),
            cp_full(2, 3, 0.1845703125, 0.3879238667654693, 61.0, 0.0, 1.0, 1.0),
            cp_full(3, 3, 0.60546875, 0.4633553246018661, -47.0, -59.0, 0.849, 1.73),
            cp_full(4, 3, 1.0, 0.6214021886400309, -33.0, 0.0, 0.377, 1.604),
            cp(0, 4, -1.0, 1.0), cp_full(1, 4, -0.5, 1.0, 0.0, -73.0, 1.0, 1.0),
            cp_full(2, 4, -0.3271484375, 1.0, 0.0, -24.0, 0.314, 2.704),
            cp(3, 4, 0.5, 1.0), cp(4, 4, 1.0, 1.0),
        ],
    ));
    // 5: 5×5
    out.push((
        5,
        5,
        vec![
            cp(0, 0, -1.0, -1.0), cp_full(1, 0, -0.6393, -1.0, 0.0, 0.0, 1.0, 2.3884),
            cp(2, 0, 0.0, -1.0), cp(3, 0, 0.5, -1.0), cp(4, 0, 1.0, -1.0),
            cp(0, 1, -1.0, -0.2301),
            cp_full(1, 1, -0.6934, -0.331, 0.0, -0.7188, 1.0, 1.063),
            cp_full(2, 1, -0.0082, -0.6814, -0.2583, 0.0, 1.0964, 1.0),
            cp_full(3, 1, 0.5836, -0.531, 0.7029, 0.0, 1.5466, 1.0),
            cp(4, 1, 1.0, -0.6407),
            cp_full(0, 2, -1.0, 0.2973, 0.0, 0.0, 1.8352, 1.0), cp(1, 2, -0.4082, 0.0602),
            cp_full(2, 2, -0.1803, -0.3646, -0.2998, 0.0, 1.1513, 1.0),
            cp_full(3, 2, 0.477, -0.1027, 0.8903, -0.1882, 1.0807, 0.8551),
            cp(4, 2, 1.0, -0.2973),
            cp_full(0, 3, -1.0, 0.7628, 0.0, 0.0, 2.3868, 1.0),
            cp_full(1, 3, -0.2525, 0.4814, -0.8406, -1.6199, 1.4093, 1.2215),
            cp_full(2, 3, 0.3607, 0.2814, -1.0713, -0.0529, 1.0025, 0.7611),
            cp_full(3, 3, 0.4885, 0.623, 0.0, 0.8184, 1.0, 1.2876),
            cp(4, 3, 1.0, 0.5),
            cp(0, 4, -1.0, 1.0), cp(1, 4, -0.4033, 1.0), cp(2, 4, 0.2672, 1.0),
            cp(3, 4, 0.5967, 1.0), cp(4, 4, 1.0, 1.0),
        ],
    ));
    // 6: 5×5
    out.push((
        5,
        5,
        vec![
            cp(0, 0, -1.0, -1.0), cp(1, 0, -0.2197, -1.0), cp(2, 0, 0.0197, -1.0),
            cp(3, 0, 0.8033, -1.0), cp(4, 0, 1.0, -1.0),
            cp(0, 1, -1.0, -0.5451),
            cp_full(1, 1, -0.4885, -0.4035, -1.0246, -0.2268, 1.1936, 0.8005),
            cp_full(2, 1, -0.1213, -0.2867, 0.0, -0.6981, 1.0, 0.809),
            cp_full(3, 1, 0.3246, -0.5628, 0.0, -1.2188, 1.0, 1.044),
            cp(4, 1, 1.0, -0.3292),
            cp(0, 2, -1.0, 0.1416),
            cp_full(1, 2, -0.341, -0.0142, 0.0, -0.4004, 1.0, 1.1293),
            cp_full(2, 2, -0.0393, -0.023, 0.2915, -0.373, 1.044, 0.9879),
            cp_full(3, 2, 0.3148, -0.0673, -0.7853, -0.8962, 1.4709, 1.0247),
            cp(4, 2, 1.0, 0.1912),
            cp(0, 3, -1.0, 0.5),
            cp_full(1, 3, -0.2689, 0.2743, 0.3404, -0.5248, 1.0184, 0.4391),
            cp_full(2, 3, 0.0721, 0.269, 0.5302, 0.1244, 0.6723, 0.3225),
            cp_full(3, 3, 0.4148, 0.3894, -0.6977, -0.6783, 0.8094, 0.9247),
            cp(4, 3, 1.0, 0.446),
            cp(0, 4, -1.0, 1.0), cp(1, 4, -0.7311, 1.0), cp(2, 4, 0.323, 1.0),
            cp(3, 4, 0.6393, 1.0), cp(4, 4, 1.0, 1.0),
        ],
    ));
    out
}

// ---------------------------------------------------------------------------
// BHPMesh 细分(上游 Hermite 双三次 patch,CPU 一次性烘顶)
// ---------------------------------------------------------------------------

/// 上游 H 矩阵(gl-matrix 列主元:列 0 = (2,-2,1,1)ᵀ …)
const H: [f64; 16] = [2.0, -2.0, 1.0, 1.0, -3.0, 3.0, -2.0, -1.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0];

/// 列主元转置
fn transpose(m: &[f64; 16]) -> [f64; 16] {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = m[r * 4 + c];
        }
    }
    out
}

/// 列主元矩阵乘:a·b(gl-matrix Mat4.mul)
fn mat_mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = sum;
        }
    }
    out
}

/// 列主元矩阵 × 列向量(gl-matrix Vec4.transformMat4:out = M·v)
fn mat_apply(m: &[f64; 16], v: [f64; 4]) -> [f64; 4] {
    [
        m[0] * v[0] + m[4] * v[1] + m[8] * v[2] + m[12] * v[3],
        m[1] * v[0] + m[5] * v[1] + m[9] * v[2] + m[13] * v[3],
        m[2] * v[0] + m[6] * v[1] + m[10] * v[2] + m[14] * v[3],
        m[3] * v[0] + m[7] * v[1] + m[11] * v[2] + m[15] * v[3],
    ]
}

/// 控制点(上游 ControlPoint:tangent 由 rot/scale 派生)
#[derive(Debug, Clone, Copy)]
struct ControlPoint {
    location: [f64; 2],
    u_tangent: [f64; 2],
    v_tangent: [f64; 2],
}

impl ControlPoint {
    fn from_conf(conf: &ControlPointConf, u_power: f64, v_power: f64) -> Self {
        let ur = conf.ur * std::f64::consts::PI / 180.0;
        let vr = conf.vr * std::f64::consts::PI / 180.0;
        Self {
            location: [conf.x, conf.y],
            u_tangent: [ur.cos() * u_power * conf.up, ur.sin() * u_power * conf.up],
            v_tangent: [-vr.sin() * v_power * conf.vp, vr.cos() * v_power * conf.vp],
        }
    }
}

/// 上游 meshCoefficients(axis 版本,输出 gl-matrix 列主元)
fn mesh_coefficients(
    p: &[ControlPoint],
    width: usize,
    x: usize,
    y: usize,
    axis: usize,
) -> [f64; 16] {
    let at = |cx: usize, cy: usize| &p[cy * width + cx];
    let p00 = at(x, y);
    let p01 = at(x, y + 1);
    let p10 = at(x + 1, y);
    let p11 = at(x + 1, y + 1);
    [
        p00.location[axis], p01.location[axis], p00.v_tangent[axis], p01.v_tangent[axis],
        p10.location[axis], p11.location[axis], p10.v_tangent[axis], p11.v_tangent[axis],
        p00.u_tangent[axis], p01.u_tangent[axis], 0.0, 0.0,
        p10.u_tangent[axis], p11.u_tangent[axis], 0.0, 0.0,
    ]
}

/// 上游 precomputeMatrix:out = Hᵀ · Mᵀ · H
fn precompute(m: &[f64; 16], h_t: &[f64; 16]) -> [f64; 16] {
    let mt = transpose(m);
    mat_mul(h_t, &mat_mul(&mt, &H))
}

/// 上游 updateMesh:Hermite 细分烘顶。返回 (顶点, 单元列数, 单元总数)。
///
/// 上游顶点网格坐标与 UV 约定:
/// - 顶点 (vxOffset, vy):vx = 控制点列 × 细分 + u,vy = 控制点行 × 细分 + v
/// - uvX = sX + v/tH,uvY = 1 − sY − u/tW(翻转适配 GL;
///   D3D 侧由渲染器统一处理方向)
pub fn tessellate(
    points: &[ControlPointConf],
    width: usize,
    height: usize,
) -> (Vec<MeshVertex>, u32, u32) {
    // 控制点矩阵(位置 + 切线)
    let u_power = 2.0 / (width - 1) as f64;
    let v_power = 2.0 / (height - 1) as f64;
    let controls: Vec<ControlPoint> = points
        .iter()
        .map(|conf| ControlPoint::from_conf(conf, u_power, v_power))
        .collect();

    let h_t = transpose(&H);
    let sub = SUBDIVISIONS;
    let sub_m1 = sub - 1;
    let t_w = sub_m1 * (width - 1);
    let t_h = sub_m1 * (height - 1);
    let inv_sub_m1 = 1.0 / sub_m1 as f64;
    let inv_t_h = 1.0 / t_h as f64;
    let inv_t_w = 1.0 / t_w as f64;

    // 预计算 u/v 幂次(上游 normPowers)
    let powers: Vec<[f64; 4]> = (0..sub)
        .map(|i| {
            let norm = i as f64 * inv_sub_m1;
            [norm * norm * norm, norm * norm, norm, 1.0]
        })
        .collect();

    // 顶点网格:(W-1)·sub × (H-1)·sub —— 上游 resize 后 vertexWidth=
    // (cpW-1)*sub(列), vertexHeight=(cpH-1)*sub(行);
    // setVertexData(vx, vy, …) 索引 = vx + vy*vertexWidth
    let grid_w = (width - 1) * sub;
    let grid_h = (height - 1) * sub;
    let mut vertices = vec![
        MeshVertex { pos: [0.0, 0.0], color: [1.0, 1.0, 1.0], uv: [0.0, 0.0] };
        grid_w * grid_h
    ];

    for cx in 0..width - 1 {
        for cy in 0..height - 1 {
            let mx = mesh_coefficients(&controls, width, cx, cy, 0);
            let my = mesh_coefficients(&controls, width, cx, cy, 1);
            let acc_x = precompute(&mx, &h_t);
            let acc_y = precompute(&my, &h_t);

            let s_x = cx as f64 / (width - 1) as f64;
            let s_y = cy as f64 / (height - 1) as f64;

            for u in 0..sub {
                let pu = powers[u];
                let ux = mat_apply(&acc_x, pu);
                let uy = mat_apply(&acc_y, pu);
                for (v, pv) in powers.iter().enumerate() {
                    let pv = *pv;
                    let px = pv[0] * ux[0] + pv[1] * ux[1] + pv[2] * ux[2] + pv[3] * ux[3];
                    let py = pv[0] * uy[0] + pv[1] * uy[1] + pv[2] * uy[2] + pv[3] * uy[3];
                    let uv_x = s_x + v as f64 * inv_t_h;
                    let uv_y = 1.0 - s_y - u as f64 * inv_t_w;
                    // 上游 setVertexData(vxOffset, vy):vx = cy*sub+u,
                    // vy = cx*sub+v,索引 = vx + vy*vertexWidth
                    let vx = cy * sub + u;
                    let vy = cx * sub + v;
                    vertices[vx + vy * grid_w] = MeshVertex {
                        pos: [px as f32, py as f32],
                        color: [1.0, 1.0, 1.0],
                        uv: [uv_x as f32, uv_y as f32],
                    };
                }
            }
        }
    }

    // 单元数 = 顶点数 - 1(上游 ELEMENT 网格:vertexWidth-1 × vertexHeight-1)。
    // 渲染器按 grid_width = cells_across + 1 寻址顶点行,恰好等于存储行宽
    // grid_w —— 此前误传顶点列数,导致 shader 行错位 + 边缘越界读零。
    let cells_across = (grid_w - 1) as u32;
    let cell_count = (grid_w as u32 - 1) * (grid_h as u32 - 1);
    (vertices, cells_across, cell_count)
}

/// 从专辑封面构建一份 mesh 背景资源(id 供缓存失效)
pub fn build_resource(cover_bytes: &[u8], id: u64) -> Option<MeshBackgroundResource> {
    let cover = image::load_from_memory(cover_bytes).ok()?.to_rgba8();
    let texture = process_album(cover);
    let (w, h) = (texture.width(), texture.height());

    // 上游 createMeshState:80% 预设随机一组,20% 随机生成 6×6
    let mut rng = MeshRng::new(id ^ 0x9E3779B97F4A7C15);
    let (points, pw, ph) = if rng.next_f64() > 0.8 {
        (generate_control_points(6, 6, &mut rng), 6, 6)
    } else {
        let presets = control_point_presets();
        let index = (rng.next_f64() * presets.len() as f64) as usize % presets.len();
        let (w, h, conf) = presets[index].clone();
        (conf, w, h)
    };
    let (vertices, cells_across, cell_count) = tessellate(&points, pw, ph);
    Some(MeshBackgroundResource {
        id,
        vertices: Arc::new(vertices),
        cells_across,
        cell_count,
        texture_width: w,
        texture_height: h,
        texture: Arc::new(texture.into_raw()),
    })
}

impl MeshBackgroundResource {
    /// 转为 gpui 场景原语(渲染器 patch 的 MeshBackground;sin/cos/
    /// volume/alpha 由调用方每帧推进——u_time = frame_time/10000,
    /// angle = (u_time + volume)·2,volume = 低频/10)
    pub fn to_scene_background(
        &self,
        time_ms: f64,
        volume: f64,
        alpha: f64,
    ) -> gpui::MeshBackground {
        let u_time = time_ms / 10000.0;
        let angle = (u_time + volume) * 2.0;
        gpui::MeshBackground {
            id: self.id,
            vertices: std::sync::Arc::new(
                self.vertices
                    .iter()
                    .map(|v| gpui::MeshBackgroundVertex {
                        pos: v.pos,
                        color: v.color,
                        uv: v.uv,
                    })
                    .collect(),
            ),
            cells_across: self.cells_across,
            cells_down: (self.cell_count / self.cells_across.max(1)).max(1),
            texture: self.texture.clone(),
            texture_width: self.texture_width,
            texture_height: self.texture_height,
            sin_angle: angle.sin() as f32,
            cos_angle: angle.cos() as f32,
            volume: volume as f32,
            alpha: alpha as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flac_cover_extraction_parses_picture_block() {
        let mut bytes = vec![b'f', b'L', b'a', b'C'];
        // PICTURE 载荷:类型(4) mimeLen(4) mime(10) descLen(4) desc(0)
        //            图形头 16 字节 + dataLen(4) + 数据(9)
        let meta_len = (4 + 4 + 10 + 4 + 0 + 16 + 4 + 9) as u32;
        bytes.push(0x86); // 最后一块 + 类型 6
        // FLAC 块长是 24 位(3 字节),高字节丢弃
        bytes.extend_from_slice(&meta_len.to_be_bytes()[1..4]);
        bytes.extend_from_slice(&3u32.to_be_bytes()); // 图片类型
        bytes.extend_from_slice(&(b"image/jpeg".len() as u32).to_be_bytes());
        bytes.extend_from_slice(b"image/jpeg");
        bytes.extend_from_slice(&0u32.to_be_bytes()); // 描述长
        bytes.extend_from_slice(&[0; 16]); // 宽高位深颜色数
        bytes.extend_from_slice(&9u32.to_be_bytes()); // 数据长
        bytes.extend_from_slice(b"COVERDATA");
        let extracted = extract_flac_cover(&bytes).expect("应提取到封面");
        assert_eq!(extracted, b"COVERDATA");
        assert!(extract_flac_cover(b"not flac").is_none());
    }

    #[test]
    fn album_pipeline_outputs_valid_32x32() {
        // 渐变测试图:左红右蓝
        let img = image::RgbaImage::from_fn(64, 64, |x, _| {
            image::Rgba([if x < 32 { 200 } else { 40 }, 40, if x < 32 { 40 } else { 200 }, 255])
        });
        let out = process_album(img);
        assert_eq!((out.width(), out.height()), (32, 32));
        for px in out.pixels() {
            assert!(px.0[3] > 0, "alpha 保持");
        }
    }

    #[test]
    fn stack_blur_preserves_flat_image() {
        let mut px = vec![100u8; 4 * 8 * 8];
        stack_blur(&mut px, 8, 8, 2, 4);
        assert!(px.iter().all(|&v| (v as i32 - 100).abs() <= 1), "平坦图应保持");
    }

    #[test]
    fn presets_match_upstream_counts() {
        let presets = control_point_presets();
        assert_eq!(presets.len(), 6, "上游六组预设");
        for (w, h, conf) in &presets {
            assert_eq!(conf.len(), w * h);
            assert!([5, 4].contains(w) && [5, 4].contains(h));
        }
    }

    #[test]
    fn tessellation_grid_matches_upstream_layout() {
        // 单 patch(2×2 控制点等距无旋转)细分:顶点数 = (1·50)²
        let conf = vec![cp(0, 0, -1.0, -1.0), cp(1, 0, 1.0, -1.0), cp(0, 1, -1.0, 1.0), cp(1, 1, 1.0, 1.0)];
        let (vertices, cells_across, cell_count) = tessellate(&conf, 2, 2);
        assert_eq!(vertices.len(), 50 * 50);
        // 单元数 = 顶点数 - 1(渲染器 grid_width = cells_across+1 = 顶点行宽)
        assert_eq!(cells_across, 49);
        assert_eq!(cell_count, 49 * 49);
        // 位置在 [-1,1],uv 在 [0,1]
        for v in &vertices {
            assert!(v.pos[0] >= -1.5 && v.pos[0] <= 1.5, "pos={:?}", v.pos);
            assert!(v.uv[0] >= -1e-6 && v.uv[0] <= 1.0 + 1e-6);
            assert!(v.uv[1] >= -1e-6 && v.uv[1] <= 1.0 + 1e-6);
        }
        // 角点:网格 [0][0] 与 [grid][grid] 位置贴角
        let tl = vertices[0];
        assert!(tl.pos[0] < -0.99 && tl.pos[1] < -0.99, "左下角(上游 GL 约定){:?}", tl.pos);
    }

    #[test]
    fn generated_control_points_stay_in_grid() {
        let mut rng = MeshRng::new(42);
        let conf = generate_control_points(6, 6, &mut rng);
        assert_eq!(conf.len(), 36);
        for p in &conf {
            assert!(p.x >= -1.6 && p.x <= 1.6 && p.y >= -1.6 && p.y <= 1.6, "{:?}", p);
        }
        // 同种子稳定
        let conf2 = generate_control_points(6, 6, &mut MeshRng::new(42));
        assert_eq!(conf, conf2);
    }
}
