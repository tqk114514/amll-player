# gpui-pre 0.3.9 本地补丁说明

vendored 自 crates.io 的 `gpui-pre@0.3.9`(gpui-kit 0.7.3 锁定的快照),
仅做最小改动:**暴露绘制期的元素变换**与**水平 alpha 渐变遮罩**。

## 背景

- 渲染核心完整支持每精灵变换:`TransformationMatrix`(scene.rs)挂在
  `MonochromeSprite`/`SubpixelSprite` 上,各平台渲染器原生应用
  (Windows D3D11 见 shaders.hlsl 的 `to_device_position_transformed`)。
- 但 facade 层没把它接到文字:`Window::paint_glyph` 内部硬编码
  `TransformationMatrix::unit()`,公开 API 也没有设置入口。
- AMLL 歌词页的行缩放动画(上游为 CSS `transform: scale()`,文字排版
  静态、GPU 缩放)需要等价能力;字号线插值会造成逐帧重排版微抖,
  两层交叉淡化则有"叠影"观感(实测被否决)。

## 变更点(全部带 `[AMLL patch]` 注释,可 grep 定位)

`src/window.rs`:

1. `Window` 新增字段 `element_transformation: TransformationMatrix`
   (与 `element_opacity` 同构的绘制期作用域栈;构造器初始化为 unit)。
2. 新增 `pub fn with_element_transformation(&mut self, transformation, f)`:
   作用域内复合当前栈(`previous.compose(new)`)后执行闭包,结束恢复;
   `pub fn element_transformation()` 访问器。
3. `paint_glyph` 的 `SubpixelSprite`/`MonochromeSprite` 插入处,
   `transformation: TransformationMatrix::unit()` 改为
   `transformation: self.element_transformation()`。
4. `PlatformWindow` trait 新增默认返回 None 的
   `capture_frame_pixels()`,`Window` 暴露同名包装——读回最近呈现的
   一帧像素,供像素级追踪(渲染目标在下一帧绘制前仍保留上一帧内容,
   逐帧调用即得呈现输出的连续记录)。

历史上曾有的第 5 处补丁(稳定光栅:固定相位 + 分数位置采样)已回滚:
分数采样会在图集瓦片边缘混入相邻字形的像素,表现为字间白色细线;
正确解法是把滚动偏移也并入绘制期变换、布局完全静态,字形绘制位置
恒定后上游默认光栅化天然稳定,无需任何特殊处理。

## 变更点(二):水平 alpha 渐变遮罩(CSS mask-image 等价)

`src/scene.rs` + `src/window.rs`(带 `[AMLL patch]` 注释):

1. `scene.rs` 新增 `SpriteGradient`(x_start/x_end/alpha_left/alpha_right,
   `#[repr(C)]` 16 字节),`MonochromeSprite`/`SubpixelSprite` 各追加
   `gradient` 字段(追加在结构体末尾,两平台 HLSL 布局镜像同步)。
2. `Window` 新增 `element_mask_gradient_stack` 与
   `pub fn with_element_mask_gradient(x_start, x_end, alpha_left, alpha_right, f)`
   (与 with_element_transformation 同构的绘制期作用域;坐标取逻辑像素,
   入栈时按 scale_factor 换算设备像素,与精灵 bounds 同空间)。
   `paint_glyph` 的两种文字精灵与 `paint_svg` 的 MonochromeSprite 插入处
   携带 `self.element_mask_gradient()`。

上游 AMLL 的逐词揭示是 CSS `mask-image: linear-gradient(to right,
bright …, dark …)`:合成器把文字内容逐像素乘以渐变遮罩 alpha。此前
用"内容裁剪切片 + 阶梯 alpha"逼近,1px/片仍有肉眼可见的带状;改为
精灵携带渐变、像素着色器逐像素插值后与 Web 渲染逐像素等价,顺带把
每词 20+ 次遮罩重绘降为单次绘制。

## 变更点(三):quad 携带元素变换

`src/scene.rs` + `src/window.rs`(带 `[AMLL patch]` 注释):

1. `scene.rs` 的 `Quad` 追加 `transformation: TransformationMatrix` 字段
   (追加于 160 字节偏移,总尺寸 184 = 160 + float2x2(16) + float2(8),
   与平台 HLSL 布局逐字节一致;gpui-player 侧有尺寸守卫测试)。
2. `paint_quad` 读取 `element_transformation()`:单位矩阵路径保持原
   `snap_bounds`/`snap_border_widths` 像素吸附不变;变换作用域内不吸附
   (仅换算设备像素),亚像素精度经矩阵在顶点阶段应用。

背景:quad 的 `snap_bounds` 会把几何吸附到设备像素,慢速缩放动画
(如间奏圆点呼吸,≈0.24px/帧)表现为周期性 1px 跳变的微动;文字精灵
走矩阵无此问题。SDF 在局部空间求值,带内 AA 随缩放等比展宽。

## 变更点(四):inactive 降频按"完全遮挡"而非"焦点"判定

`src/platform.rs` + `src/window.rs`(带 `[AMLL patch]` 注释):

1. `PlatformWindow` 新增默认返回 false 的 `is_render_obstructed()`
   ——窗口渲染是否被完全遮挡(最小化,或被其他窗口完全盖住)。
2. 帧率策略(原"窗口未聚焦 → ~30fps 节流省电")的判定由
   `!active` 改为调用 `is_render_obstructed()`:失焦但可见的窗口
   仍全速渲染(音乐播放器常驻后台需保持流畅);输入高频例外与
   热节流分支保留,窗体 `active` 字段不再被帧循环消费。

## 变更点(五):字形高斯模糊作用域(CSS filter: blur 等价)

`src/window.rs`(带 `[AMLL patch]` 注释):新增 `with_element_blur(
blur_radius, f)` 与 `element_blur()`(与 with_element_transformation
同构的绘制期作用域;逻辑像素入栈时换算设备像素,嵌套内层优先)。
`paint_glyph`/`paint_svg` 把当前半径以 f32 位模式写入精灵的 `pad`
对齐填充位——结构体尺寸零变更(128 守卫测试原样有效)。

数学依据:高斯卷积线性,blur(Σ字形)=Σblur(字形),逐字形模糊与
整层模糊等价;字形光晕仅来自自身瓦片内墨迹,瓦片外采样置零恰为
线性卷积的正确贡献——无需离屏层与分离模糊两趟,无降级近似。
blur_radius 即高斯标准差(CSS blur(Npx) 的 N 定义为 σ,MDN 原文:
"它定义了高斯函数的标准偏差值")。

## 变更点(六):MeshBackground 场景原语(上游 MeshGradientRenderer)

`src/scene.rs` + `src/window.rs`(带 `[AMLL patch]` 注释):新增
`MeshBackground` 场景字段(每帧最多一份,最先绘制)与
`Window::paint_mesh_background()` API(paint/prepaint 阶段设置,与
作用域 API 同构断言)。顶点(Hermite 细分,pos2+color3+uv2)、纹理
(RGBA8 32×32 处理后专辑图)、cells_across/down 由 CPU 侧(上游
mesh-renderer 数据管线移植)提供;sin/cos/volume/alpha 每帧推进。

## 已知边界

- `paint_emoji` 走 `PolychromeSprite`(全彩图元,无变换字段),
  表情符号在变换作用域内不参与缩放(歌词行内 emoji 属后续阶段场景)。
- 下划线/删除线走 Path 图元,同样不在变换作用域内(歌词页未使用)。

## 维护

升级 gpui-kit(连带 gpui-pre 版本)时:对照新版 `src/window.rs` 的
`paint_glyph` 与 `with_element_opacity` 重新套用上述 3 处改动即可;
本目录其余文件与上游发布版一致(仅删除了未随包发布的 examples/tests 声明)。
