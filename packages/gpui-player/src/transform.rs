//! 绘制期变换包装元素。
//!
//! 依赖 `patches/gpui-pre` 暴露的 `Window::with_element_transformation`:
//! 作用域内的字形精灵携带矩阵,由平台渲染器在顶点阶段应用
//! (Windows D3D11 见上游 shaders.hlsl 的 `to_device_position_transformed`)。
//!
//! 上游 AMLL 的滚动与缩放全部走 CSS `transform`(translateY + scale):
//! 文字排版完全静态、合成层在 GPU 上移动/缩放。本元素提供等价机制——
//! 行元素恒定叠放在布局原点,滚动偏移与缩放都经矩阵应用,字形绘制
//! 位置逐帧不变,光栅天然稳定(无逐帧重光栅,亦无分数采样图集的边缘
//! 渗色)。
//!
//! 矩阵单位与上游 `Svg::into_matrix` 一致:全部在设备像素空间构造。

use gpui::{
    App, AnyElement, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Point, TransformationMatrix, Window,
};

/// 对子元素应用"绕指定点缩放 + 平移"的包装元素。
///
/// `local_origin` 是缩放中心,相对子元素自身布局原点的偏移(逻辑像素);
/// 上游歌词行的 transform-origin 是行元素左缘垂直居中,即 (0, 行高/2)。
/// `translate` 是滚动偏移(逻辑像素,如 posY),在缩放之后应用。
pub struct ScaledElement {
    inner: AnyElement,
    scale: f32,
    translate: Point<Pixels>,
    local_origin: Point<Pixels>,
    matrix: TransformationMatrix,
}

impl ScaledElement {
    pub fn new(
        inner: impl IntoElement,
        scale: f32,
        translate: Point<Pixels>,
        local_origin: Point<Pixels>,
    ) -> Self {
        Self {
            inner: inner.into_any_element(),
            scale,
            translate,
            local_origin,
            matrix: TransformationMatrix::unit(),
        }
    }
}

impl Element for ScaledElement {
    type RequestLayoutState = LayoutId;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id = self.inner.request_layout(window, cx);
        (layout_id, layout_id)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.inner.prepaint(window, cx);
        // 子元素自身的布局 bounds(静态叠放,逐帧恒定)
        let child_bounds = window.layout_bounds(*request_layout);
        // 矩阵在设备像素空间构造(与上游 Svg::into_matrix 同一约定):
        // p' = s·(p − C) + C + T,其中 C 为缩放中心、T 为滚动偏移
        let scale_factor = window.scale_factor();
        let center = (child_bounds.origin + self.local_origin).scale(scale_factor);
        let translate = self.translate.scale(scale_factor);
        let scale = self.scale;
        self.matrix = TransformationMatrix {
            rotation_scale: [[scale, 0.0], [0.0, scale]],
            translation: [
                center.x.0 * (1.0 - scale) + translate.x.0,
                center.y.0 * (1.0 - scale) + translate.y.0,
            ],
        };
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_transformation(self.matrix, |window| {
            self.inner.paint(window, cx);
        });
    }
}

impl IntoElement for ScaledElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
