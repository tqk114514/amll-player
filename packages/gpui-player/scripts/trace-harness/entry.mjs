// 采集 harness 的打包入口:整体再导出 core bundle(rolldown 会把
// gl-matrix/@pixi/bezier-easing 等裸依赖打进单个自包含文件)。
export * from "../../../player/node_modules/@applemusic-like-lyrics/core/dist/amll-core.mjs";
