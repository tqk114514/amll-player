//! CSS timing function(cubic-bezier)的数值求解。
//!
//! 上游 AMLL 的过渡/关键帧使用 CSS 缓动,其精确定义:
//! - `ease-out` = cubic-bezier(0, 0, 0.58, 1)——mask alpha 过渡
//!   (`--bright/dark-mask-alpha var(--mask-alpha-duration) ease-out`)与
//!   float 关键帧的 easing
//! - `ease` = cubic-bezier(0.25, 0.1, 0.25, 1)——wrapper opacity 过渡
//!   (`transition: opacity .4s`,未写明 timing function 时默认 ease)

/// 三次贝塞尔缓动(时间→进度):二分法解 x(s)=t 再求 y(s)
pub fn css_bezier(x1: f64, y1: f64, x2: f64, y2: f64) -> impl Fn(f64) -> f64 {
    move |t| {
        let t = t.clamp(0.0, 1.0);
        // 贝塞尔参数化:x(s) = 3(1-s)²s·x1 + 3(1-s)s²·x2 + s³
        let x_at = |s: f64| 3.0 * (1.0 - s) * (1.0 - s) * s * x1 + 3.0 * (1.0 - s) * s * s * x2 + s * s * s;
        let mut low = 0.0;
        let mut high = 1.0;
        let mut s = t;
        for _ in 0..24 {
            let x = x_at(s);
            if (x - t).abs() < 1e-7 {
                break;
            }
            if x < t {
                low = s;
            } else {
                high = s;
            }
            s = (low + high) / 2.0;
        }
        3.0 * (1.0 - s) * (1.0 - s) * s * y1 + 3.0 * (1.0 - s) * s * s * y2 + s * s * s
    }
}

/// CSS `ease-out`:cubic-bezier(0, 0, 0.58, 1)
pub fn ease_out() -> impl Fn(f64) -> f64 {
    css_bezier(0.0, 0.0, 0.58, 1.0)
}

/// CSS `ease`:cubic-bezier(0.25, 0.1, 0.25, 1)
pub fn ease() -> impl Fn(f64) -> f64 {
    css_bezier(0.25, 0.1, 0.25, 1.0)
}
