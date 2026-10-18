# gpui-pre 0.3.9 本地补丁说明

vendored 自 crates.io 的 `gpui-pre@0.3.9`(gpui-kit 0.7.3 锁定的快照),
仅做一处最小改动:**暴露绘制期的元素变换**。

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
4. `Window` 新增字段 `stable_glyph_raster: bool` 与
   `pub fn set_stable_glyph_raster(&mut self, enabled: bool)`。
5. `paint_glyph` 光栅化分支:开启稳定模式时固定亚像素相位为 (0,0)
   并保留字形原点的分数像素位置(sprite bounds 不截断),由 GPU
   双线性采样负责亚像素放置——与 WebView 位图层移动等价。
   动机:逐位置重光栅会让字形 AA 随亚像素相位逐帧变化,弹簧动画
   尾部速度低、位移小于相位桶宽度,AA 逐帧跳变表现为肉眼可见的微动。
   关闭时行为与上游一致(paint_emoji 的全彩路径不受影响)。

## 已知边界

- `paint_emoji` 走 `PolychromeSprite`(全彩图元,无变换字段),
  表情符号在变换作用域内不参与缩放(歌词行内 emoji 属后续阶段场景)。
- 下划线/删除线走 Path 图元,同样不在变换作用域内(歌词页未使用)。

## 维护

升级 gpui-kit(连带 gpui-pre 版本)时:对照新版 `src/window.rs` 的
`paint_glyph` 与 `with_element_opacity` 重新套用上述 3 处改动即可;
本目录其余文件与上游发布版一致(仅删除了未随包发布的 examples/tests 声明)。
