# 上游原始源码(对照用)

从 `@applemusic-like-lyrics/core@0.6.0` 的发布 sourcemap
(`dist/amll-core.mjs.map` 的 `sourcesContent`)还原出的原始 TypeScript
源码,53 个文件,目录结构以 `__` 替代原路径分隔符。

用途:逐词揭示、间奏圆点、行激活过渡、优化管线等机制的"正统实现"
对照基准(上游源码与 dist 反汇编相比无变量名丢失与内联展开)。
本目录是自动还原物,勿手改;升级上游依赖后可重新解包刷新。
