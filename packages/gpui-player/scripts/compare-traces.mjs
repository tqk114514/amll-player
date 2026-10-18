#!/usr/bin/env node
// 数值对比:GPUI 原型 trace 与上游 DOM 采集,逐帧逐行 diff。
//
// 输入:两份 JSONL(每行一个 JSON 对象;ev 字段为事件,否则为帧记录)
//   帧字段:media, y[], s[], a[]/maskBright+op, scrollTo, k, d, interludeY, seek
// 用法: node packages/gpui-player/scripts/compare-traces.mjs <gpui.jsonl> <web.jsonl> [--report out.md]
//
// 对齐方式:按 media(歌曲时间)线性插值重采样到 10ms 公共网格。

import { readFileSync, writeFileSync } from "node:fs";

const args = process.argv.slice(2);
const files = [];
let reportPath = null;
for (let i = 0; i < args.length; i++) {
	if (args[i] === "--report") reportPath = args[++i];
	else files.push(args[i]);
}
if (files.length < 2) {
	console.error("用法: compare-traces.mjs <gpui.jsonl> <web.jsonl> [--report out.md]");
	process.exit(1);
}

function parse(path) {
	const frames = [];
	const events = [];
	for (const line of readFileSync(path, "utf8").split("\n")) {
		const text = line.trim();
		if (!text) continue;
		let value;
		try {
			value = JSON.parse(text);
		} catch {
			continue;
		}
		if (value.ev) events.push(value);
		else frames.push(value);
	}
	frames.sort((a, b) => a.media - b.media);
	// 剧本含回跳 seek 时同一 media 区间会被两遍播放各记录一次:
	// 按 media 去重,保留墙钟较晚的记录(该遍的最终状态)
	const deduped = [];
	for (const frame of frames) {
		if (deduped.length > 0 && deduped.at(-1).media === frame.media) {
			if (frame.wall >= deduped.at(-1).wall) deduped[deduped.length - 1] = frame;
		} else deduped.push(frame);
	}
	return { frames: deduped, events };
}

const gpui = parse(files[0]);
const web = parse(files[1]);

const lineCount = Math.min(
	gpui.frames.at(-1)?.y.length ?? 0,
	web.frames.at(-1)?.y.length ?? 0,
);
if (lineCount === 0) {
	console.error("行数为 0,检查输入文件");
	process.exit(1);
}

const gridFrom = Math.max(
	gpui.frames[0].media,
	web.frames[0].media,
);
const gridTo = Math.min(
	gpui.frames.at(-1).media,
	web.frames.at(-1).media,
);
const STEP = 10;

function resample(frames, key, lineIndex, media) {
	// 帧按 media 升序(剧本只含前进 seek);线性插值
	let lo = 0;
	let hi = frames.length - 1;
	if (media <= frames[0].media) return frames[0][key]?.[lineIndex] ?? null;
	if (media >= frames.at(-1).media) return frames.at(-1)[key]?.[lineIndex] ?? null;
	while (hi - lo > 1) {
		const mid = (lo + hi) >> 1;
		if (frames[mid].media <= media) lo = mid;
		else hi = mid;
	}
	const a = frames[lo];
	const b = frames[hi];
	const va = a[key]?.[lineIndex];
	const vb = b[key]?.[lineIndex];
	if (va == null || vb == null) return va ?? vb ?? null;
	const t = (media - a.media) / (b.media - a.media || 1);
	return va + (vb - va) * t;
}

function scalar(frames, key, media) {
	let lo = 0;
	let hi = frames.length - 1;
	if (media <= frames[0].media) return frames[0][key];
	if (media >= frames.at(-1).media) return frames.at(-1)[key];
	while (hi - lo > 1) {
		const mid = (lo + hi) >> 1;
		if (frames[mid].media <= media) lo = mid;
		else hi = mid;
	}
	const a = frames[lo];
	const b = frames[hi];
	const t = (media - a.media) / (b.media - a.media || 1);
	return a[key] + (b[key] - a[key]) * t;
}

// 逐行统计 y / scale
function seriesDiff(key, gpuiKey) {
	const stats = [];
	for (let line = 0; line < lineCount; line++) {
		let maxDiff = 0;
		let sumDiff = 0;
		let count = 0;
		let worstMedia = 0;
		let worstG = 0;
		let worstW = 0;
		for (let media = gridFrom; media <= gridTo; media += STEP) {
			const g = resample(gpui.frames, gpuiKey, line, media);
			const w = resample(web.frames, key, line, media);
			if (g == null || w == null) continue;
			const diff = Math.abs(g - w);
			if (diff > maxDiff) {
				maxDiff = diff;
				worstMedia = media;
				worstG = g;
				worstW = w;
			}
			sumDiff += diff;
			count++;
		}
		stats.push({ line, maxDiff, meanDiff: count ? sumDiff / count : 0, worstMedia, worstG, worstW });
	}
	return stats;
}

// web 的 y/s 与 GPUI 同键;alpha 需要推导,单独处理
const yStats = seriesDiff("y", "y");
const sStats = seriesDiff("s", "s");

// 采集环境里注册属性(@property)的 CSS 过渡值不可靠,但类状态(maskCls)是权威;
// 按 CSS 语义解析推导 web 的词 mask alpha:激活 → 1.0(0.3s ease-out),失活 → 0.2(0.45s)
function deriveWebMaskAlphas(frames) {
	const lineCount = frames[0]?.maskCls?.length ?? 0;
	const alphas = frames.map(() => new Array(lineCount).fill(0.2));
	for (let line = 0; line < lineCount; line++) {
		let from = 0.2;
		let to = 0.2;
		let startMedia = -Infinity;
		let duration = 0;
		for (let i = 0; i < frames.length; i++) {
			const frame = frames[i];
			const target = frame.maskCls?.[line] === true ? 1.0 : 0.2;
			if (target !== to) {
				from = i === 0 ? to : alphas[i - 1][line];
				to = target;
				startMedia = frame.media;
				duration = to > from ? 300 : 450;
			}
			const t = duration > 0 ? Math.min(1, (frame.media - startMedia) / duration) : 1;
			const eased = 1 - (1 - t) ** 3; // CSS ease-out 的近似(与 GPUI 侧一致)
			alphas[i][line] = from + (to - from) * eased;
		}
	}
	return alphas;
}

const webMaskAlphas = deriveWebMaskAlphas(web.frames);

function resamplePairs(pairs, media) {
	// pairs: 按 media 升序的 [media, value]
	let lo = 0;
	let hi = pairs.length - 1;
	if (media <= pairs[0][0]) return pairs[0][1];
	if (media >= pairs.at(-1)[0]) return pairs.at(-1)[1];
	while (hi - lo > 1) {
		const mid = (lo + hi) >> 1;
		if (pairs[mid][0] <= media) lo = mid;
		else hi = mid;
	}
	const [ma, va] = pairs[lo];
	const [mb, vb] = pairs[hi];
	const t = (media - ma) / (mb - ma || 1);
	return va + (vb - va) * t;
}

const aStats = [];
for (let line = 0; line < lineCount; line++) {
	let maxDiff = 0;
	let sumDiff = 0;
	let count = 0;
	let worstMedia = 0;
	let worstG = 0;
	let worstW = 0;
	const webPairs = web.frames.map((frame, index) => [
		frame.media,
		(frame.op?.[line] ?? 1) * webMaskAlphas[index][line],
	]);
	for (let media = gridFrom; media <= gridTo; media += STEP) {
		const g = resample(gpui.frames, "a", line, media);
		const w = resamplePairs(webPairs, media);
		if (g == null || w == null) continue;
		const diff = Math.abs(g - w);
		if (diff > maxDiff) {
			maxDiff = diff;
			worstMedia = media;
			worstG = g;
			worstW = w;
		}
		sumDiff += diff;
		count++;
	}
	aStats.push({ line, maxDiff, meanDiff: count ? sumDiff / count : 0, worstMedia, worstG, worstW });
}

// scrollTo 序列对比
let scrollToMismatch = 0;
let scrollToSamples = 0;
const scrollToDiffTimes = [];
for (let media = gridFrom; media <= gridTo; media += STEP) {
	const g = Math.round(scalar(gpui.frames, "scrollTo", media));
	const w = Math.round(scalar(web.frames, "scrollTo", media));
	scrollToSamples++;
	if (g !== w) {
		scrollToMismatch++;
		if (scrollToDiffTimes.length < 8) scrollToDiffTimes.push({ media, gpui: g, web: w });
	}
}

// 事件对比(过滤 resize:上游高度测量会零散触发 resize 重排,语义上无影响)
const gpuiRetargets = gpui.events.filter((event) => event.ev === "retarget");
const webRetargets = web.events.filter(
	(event) => event.ev === "retarget" && event.reason !== "resize",
);

function fmt(value, digits = 2) {
	return Number(value).toFixed(digits);
}

const report = [];
report.push("# AMLL GPUI 原型 vs 上游 DOM 数值对比");
report.push("");
report.push(
	`- GPUI:${gpui.frames.length} 帧,${gpuiRetargets.length} 次排版事件,media ${fmt(gpui.frames[0].media, 0)}–${fmt(gpui.frames.at(-1).media, 0)}ms`,
);
report.push(
	`- Web: ${web.frames.length} 帧,${webRetargets.length} 次排版事件,media ${fmt(web.frames[0].media, 0)}–${fmt(web.frames.at(-1).media, 0)}ms`,
);
report.push(`- 公共对比区间:${fmt(gridFrom, 0)}–${fmt(gridTo, 0)}ms,步长 ${STEP}ms,共 ${Math.floor((gridTo - gridFrom) / STEP) + 1} 个采样点`);
report.push("");

report.push("## 焦点行(scrollTo)");
report.push(
	`- 不一致率:${fmt((100 * scrollToMismatch) / Math.max(1, scrollToSamples), 2)}% (${scrollToMismatch}/${scrollToSamples})`,
);
if (scrollToDiffTimes.length > 0) {
	report.push(`- 首个不一致采样:${scrollToDiffTimes.map((item) => `media=${fmt(item.media, 0)} gpui=${item.gpui} web=${item.web}`).join("; ")}`);
}
report.push("");

const sections = [
	["纵向位置 y(px)", yStats],
	["缩放 s(%)", sStats],
	["文本 alpha", aStats],
];
for (const [title, stats] of sections) {
	report.push(`## ${title}`);
	report.push("");
	report.push("| 行 | 最大差 | 平均差 | 最差时刻 media | GPUI | Web |");
	report.push("| --: | --: | --: | --: | --: | --: |");
	for (const stat of stats) {
		report.push(
			`| ${stat.line} | ${fmt(stat.maxDiff, 3)} | ${fmt(stat.meanDiff, 3)} | ${fmt(stat.worstMedia, 0)} | ${fmt(stat.worstG, 2)} | ${fmt(stat.worstW, 2)} |`,
		);
	}
	report.push("");
}

report.push("## 排版事件");
report.push("| # | GPUI media | reason | k | d | stagger | Web media | reason | k | d |");
report.push("| --: | --: | :-: | --: | --: | :-: | --: | :-: | --: | --: |");
const eventRows = Math.max(gpuiRetargets.length, webRetargets.length);
for (let i = 0; i < Math.min(eventRows, 40); i++) {
	const g = gpuiRetargets[i];
	const w = webRetargets[i];
	report.push(
		`| ${i} | ${g ? fmt(g.media, 0) : "-"} | ${g?.reason ?? "-"} | ${g ? fmt(g.k, 1) : "-"} | ${g ? fmt(g.d, 1) : "-"} | ${g && g.stagger ? "✓" : g ? "✗" : "-"} | ${w ? fmt(w.media, 0) : "-"} | ${w?.reason ?? "-"} | ${w ? fmt(w.k, 1) : "-"} | ${w ? fmt(w.d, 1) : "-"} |`,
	);
}
report.push("");

// 稳态位置对比:每行取其成为焦点后 1.5s 处的 y(两侧行为应一致)
report.push("## 稳态位置(每行成为焦点后 1500ms)");
report.push("");
report.push("| 行 | media | GPUI y | Web y | Δ |");
report.push("| --: | --: | --: | --: | --: |");
// 用 web 的 scrollTo 序列推断各行焦点的起始 media
const steady = [];
for (let line = 0; line < lineCount; line++) {
	// 该行开始时间附近(用 demo-lines 的 startTime 不可得,改用 web scrollTo 序列)
	let focusStart = null;
	for (const frame of web.frames) {
		if (frame.scrollTo === line) {
			focusStart = frame.media;
			break;
		}
	}
	if (focusStart == null) continue;
	const media = focusStart + 1500;
	if (media > gridTo) continue;
	const g = resample(gpui.frames, "y", line, media);
	const w = resample(web.frames, "y", line, media);
	steady.push({ line, media, g, w });
	report.push(`| ${line} | ${fmt(media, 0)} | ${fmt(g, 2)} | ${fmt(w, 2)} | ${fmt(Math.abs(g - w), 2)} |`);
}
report.push("");

const text = report.join("\n");
console.log(text);
if (reportPath) {
	writeFileSync(reportPath, text, "utf8");
	console.error(`\n报告已写入: ${reportPath}`);
}
