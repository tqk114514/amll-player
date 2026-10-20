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

## 变更点(四):文字精灵的水平 alpha 渐变遮罩

`src/shaders.hlsl`(带 `[AMLL patch]` 注释):`MonochromeSprite` 结构体
追加 `float4 gradient`(与 gpui-pre 补丁的 Rust `SpriteGradient` 布局
逐字节一致,追加在 112 字节偏移,总尺寸 128);`monochrome_sprite_vertex`
新增插值输出 `local_x`(局部空间设备像素,变换前——与 CSS mask 在
transform 下定义于元素空间的语义一致)与恒定 `mask_gradient`;
`monochrome_sprite_fragment`/`subpixel_sprite_fragment` 按
`sprite_mask_gradient_alpha` 逐像素线性插值乘入 alpha。

这是 CSS `mask-image: linear-gradient` 的合成器等价实现:带内逐像素
线性插值、带外取端值,替代先前的内容裁剪切片逼近(有可见带状)。

## 变更点(五):quad 携带元素变换

`src/shaders.hlsl`(带 `[AMLL patch]` 注释):`Quad` 结构体追加
`TransformationMatrix transformation`(160 字节偏移,总尺寸 184 与
Rust 侧逐字节一致);`quad_vertex` 顶点经 `to_device_position_transformed`
到设备空间、裁剪在局部空间(与文字精灵裁剪修复同一语义);
`quad_fragment` 以 `quad_local_point`(逆矩阵,行向量约定与正向变换
`mul(position, RS) + T` 成对)把像素位置映射回局部空间求 SDF 与渐变,
单位矩阵时与原路径逐位一致。

这是 quad `snap_bounds` 像素吸附的亚像素修正:变换作用域内几何
不吸附,慢速缩放动画(间奏圆点呼吸)不再出现 1px 级跳变。

## 变更点(六):完全遮挡检测(is_render_obstructed)

`src/window.rs`(带 `[AMLL patch]` 注释):实现
`PlatformWindow::is_render_obstructed` = `IsIconic` || z-order 遮盖
累计检测(Chromium NativeWindowOcclusionTracker 的简化)——自本窗口
向上遍历 z 序,剔除 cloak(其他虚拟桌面)与 alpha=0 的分层窗口,
累计其余窗口对本窗口(经所在监视器裁剪)矩形的覆盖,剩余可见
面积 ≤ 0 即完全遮挡。判据为纯几何 + z 序,与是否最大化无关
(最大化窗口在播放窗之下时根本不在遍历链上);无状态,逐帧成本
微秒级,判定翻转时 log::info 输出遮盖率供实测核对。配合 gpui-pre 的
帧率策略:仅最小化/完全遮挡才降频,失焦可见全速。

## 维护

升级 gpui-kit(连带 gpui-pre-windows 版本)时,对照新版
`directx_renderer.rs` 的 `fn present` 重新套用这一处改动即可;
其余文件与上游发布版一致(未随包发布的 Cargo.lock/Cargo.toml.orig
未纳入)。
