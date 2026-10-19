# gpui-pre-windows 0.3.9 本地补丁说明

vendored 自 crates.io 的 `gpui-pre-windows@0.3.9`(Windows D3D11/DirectWrite
平台层,gpui-kit 0.7.3 依赖链),仅一处改动。

## 变更点(`src/directx_renderer.rs`,带 `[AMLL patch]` 注释)

`DirectXRenderer::present`:`Present(0, …)` 改为 `Present(1, …)`
(SyncInterval=1,等待垂直同步)。

上游用 SyncInterval=0(不等待),配合 gpui 无节流的动画循环,
实测渲染循环以 ~200fps 过载 FLIP_SEQUENTIAL(3 缓冲)翻转队列;DWM
按刷新率合成时采样相位漂移,动画尾部的显示帧距不均匀,表现为肉眼
可见的"轻微微动"。锁到刷新率后,渲染/动画/呈现与 WebView 合成器
的节奏一致(歌词页为 60Hz 内容)。

## 变更点(二):像素读回

`src/directx_renderer.rs` 新增 `pub fn capture_last_frame()`(带
`[AMLL patch]` 注释):把渲染目标 CopyResource 到 staging 纹理并读回
为 RGBA,不重绘、不影响呈现;`src/window.rs` 的 `WindowsWindow` 经
`PlatformWindow::capture_frame_pixels`(gpui-pre 补丁)对外暴露。
仅诊断用途。

## 变更点(三):文字精灵裁剪空间

`src/shaders.hlsl` 的 `monochrome_sprite_vertex`(含 subpixel 复用):
content_mask 的裁剪测试由 `distance_from_clip_rect_transformed`
(变换后位置 vs 未变换遮罩)改为 `distance_from_clip_rect`
(局部位置 vs 局部遮罩)。

上游的变换后测试在 `with_element_transformation` 作用域内会因
矩阵平移把被 with_content_mask 遮罩的文字全部裁空(遮罩在绘制空间
定义、位置却已在视觉空间);遮罩与 bounds 同处一个坐标空间,局部
测试才是 `with_content_mask` 的正确语义。AMLL 的逐词渐变揭示
(内容裁剪切片 × 绘制期变换)依赖此修复,否则激活行整行不可见。
旋转 SVG 等元素宽祖先遮罩的小幅变换场景下两种测试差异可忽略。

## 维护

升级 gpui-kit(连带 gpui-pre-windows 版本)时,对照新版
`directx_renderer.rs` 的 `fn present` 重新套用这一处改动即可;
其余文件与上游发布版一致(未随包发布的 Cargo.lock/Cargo.toml.orig
未纳入)。
