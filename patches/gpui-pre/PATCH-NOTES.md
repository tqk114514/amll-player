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
4. `PlatformWindow` trait 新增默认返回 None 的
   `capture_frame_pixels()`,`Window` 暴露同名包装——读回最近呈现的
   一帧像素,供像素级追踪(渲染目标在下一帧绘制前仍保留上一帧内容,
   逐帧调用即得呈现输出的连续记录)。

历史上曾有的第 5 处补丁(稳定光栅:固定相位 + 分数位置采样)已回滚:
分数采样会在图集瓦片边缘混入相邻字形的像素,表现为字间白色细线;
正确解法是把滚动偏移也并入绘制期变换、布局完全静态,字形绘制位置
恒定后上游默认光栅化天然稳定,无需任何特殊处理。

## 已知边界

- `paint_emoji` 走 `PolychromeSprite`(全彩图元,无变换字段),
  表情符号在变换作用域内不参与缩放(歌词行内 emoji 属后续阶段场景)。
- 下划线/删除线走 Path 图元,同样不在变换作用域内(歌词页未使用)。

## 维护

升级 gpui-kit(连带 gpui-pre 版本)时:对照新版 `src/window.rs` 的
`paint_glyph` 与 `with_element_opacity` 重新套用上述 3 处改动即可;
本目录其余文件与上游发布版一致(仅删除了未随包发布的 examples/tests 声明)。
