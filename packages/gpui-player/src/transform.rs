//! 绘制期变换包装元素。
//!
//! 依赖 `patches/gpui-pre` 暴露的 `Window::with_element_transformation`:
//! 作用域内的字形精灵携带矩阵,由平台渲染器在顶点阶段应用
//! (Windows D3D11 见上游 shaders.hlsl 的 `to_device_position_transformed`)。
//!
//! 上游 AMLL 对歌词行的缩放是 CSS `transform: scale()`——文字排版完全
//! 静态、GPU 缩放已合成层。本元素提供等价机制:子元素(静态排版文字)
//! 在 paint 阶段按矩阵缩放,不触发任何重排版。
//!
//! 矩阵单位与上游 `Svg::into_matrix` 一致:全部在设备像素空间构造。

use gpui::{
    App, AnyElement, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Point, TransformationMatrix, Window,
};

/// 对子元素应用"绕指定点缩放"的包装元素。
///
/// `local_origin` 是缩放中心,相对子元素自身布局原点的偏移(逻辑像素);
/// 上游歌词行的 transform-origin 是行元素左缘垂直居中,即 (0, 行高/2)。
pub struct ScaledElement {
    inner: AnyElement,
    scale: f32,
    local_origin: Point<Pixels>,
    matrix: TransformationMatrix,
}

impl ScaledElement {
    pub fn new(inner: impl IntoElement, scale: f32, local_origin: Point<Pixels>) -> Self {
        Self {
            inner: inner.into_any_element(),
            scale,
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
        // 子元素自身的布局 bounds(绝对定位的 left/top 已含在内)
        let child_bounds = window.layout_bounds(*request_layout);
        // 矩阵在设备像素空间构造(与上游 Svg::into_matrix 同一约定)
        let scale_factor = window.scale_factor();
        let center = (child_bounds.origin + self.local_origin).scale(scale_factor);
        let scale = self.scale;
        self.matrix = TransformationMatrix {
            rotation_scale: [[scale, 0.0], [0.0, scale]],
            translation: [center.x.0 * (1.0 - scale), center.y.0 * (1.0 - scale)],
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
