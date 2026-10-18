#!/usr/bin/env node
// 生成数值对比用的演示歌词(单一数据源,两个输出):
//   testdata/demo.ttml        —— GPUI 原型消费(ttml_processor 解析)
//   testdata/demo-lines.json  —— 浏览器 harness 消费(AMLL core setLyricLines 格式)
//   testdata/demo-score.json  —— 两侧共用的采集剧本(时长 + 脚本化 seek)
//
// 用法: node packages/gpui-player/scripts/gen-demo-ttml.mjs

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(here, "..", "testdata");

const fmt = (ms) => {
	const h = Math.floor(ms / 3600000);
	const m = Math.floor((ms % 3600000) / 60000);
	const s = Math.floor((ms % 60000) / 1000);
	const f = Math.floor(ms % 1000);
	return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${String(f).padStart(3, "0")}`;
};

// 歌词数据:时间均为毫秒。设计覆盖:前奏间奏(anchor=-1)、长/短行间隔
// (动态刚度跨区间)、12s 间奏(圆点展示 + spacer)、末尾播完。
const lyrics = [
	{ start: 9000, dur: 4200, text: "前奏圆点会在这九秒里呼吸" },
	{ start: 14200, dur: 3400, text: "第二行紧跟着唱起来" },
	{ start: 18000, dur: 3000, text: "间隔短一点 刚度会更高" },
	{ start: 21800, dur: 2400, text: "这一句稍微长一些,词也多一些" },
	{ start: 24400, dur: 2800, text: "短句" },
	{ start: 27500, dur: 200, text: "急促的一行" },
	{ start: 27800, dur: 3600, text: "倒数节奏开始密集" },
	{ start: 32800, dur: 6200, text: "间奏前的最后一行 唱完就休息" },
	// —— 12s 间奏 [39000, 51000):圆点演出 + spacer ——
	{ start: 51000, dur: 4600, text: "间奏之后归来" },
	{ start: 55900, dur: 4200, text: "倒数第二行" },
	{ start: 60400, dur: 5000, text: "最后一行唱完就结束了" },
];

// 每行按字符比例摊开词时序;词尾带空格(TTML 里 span 文本以空格结尾,
// ttml_processor 会标 ends_with_space,demo-lines.json 直接带尾空格)
function splitWords(line) {
	const chars = [...line.text];
	// 行尾留白按比例缩放,极短行也不至于出现词时长为负
	const tail = Math.min(520, Math.max(40, line.dur * 0.1));
	const duration = line.dur - tail;
	const words = [];
	let buffer = "";
	const groups = [];
	for (const char of chars) {
		if (char === " ") {
			if (buffer) groups.push(buffer);
			buffer = "";
		} else {
			buffer += char;
		}
	}
	if (buffer) groups.push(buffer);
	const totalLen = groups.reduce((acc, word) => acc + word.length, 0);
	let cursor = line.start;
	for (const [i, word] of groups.entries()) {
		const isLast = i === groups.length - 1;
		const wordDur = (duration * word.length) / totalLen;
		words.push({
			word: isLast ? word : `${word} `,
			startTime: Math.round(cursor),
			endTime: Math.round(cursor + wordDur),
		});
		cursor += wordDur;
	}
	return words;
}

const lines = lyrics.map((line) => {
	const words = splitWords(line);
	return {
		words,
		startTime: line.start,
		endTime: line.start + line.dur,
		translatedLyric: "",
		romanLyric: "",
		isBG: false,
		isDuet: false,
	};
});

let ttml = `<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://musicbrainz.org/ns/itunes/1.0#">
<body><div>`;
for (const line of lines) {
	ttml += `\n<p begin="${fmt(line.startTime)}" end="${fmt(line.endTime)}">`;
	for (const word of line.words) {
		ttml += `<span begin="${fmt(word.startTime)}" end="${fmt(word.endTime)}">${word.word}</span>`;
	}
	ttml += `</p>`;
}
ttml += `\n</div></body></tt>
`;

const score = {
	duration_ms: lines.at(-1).endTime + 3000,
	seeks: [
		// at_ms 写在"跳转后的媒体时间线"上:第一次跳到 56000ms 后,
		// 第二个剧本点要在跳转后的时间轴上往后写(63000ms)
		{ at_ms: 20000, to_ms: 56000 },
		{ at_ms: 63000, to_ms: 39500 },
	],
};

mkdirSync(outDir, { recursive: true });
writeFileSync(join(outDir, "demo.ttml"), ttml, "utf8");
writeFileSync(join(outDir, "demo-lines.json"), `${JSON.stringify(lines, null, "\t")}\n`, "utf8");
writeFileSync(join(outDir, "demo-score.json"), `${JSON.stringify(score, null, "\t")}\n`, "utf8");
console.log(
	`生成 ${lines.length} 行: demo.ttml / demo-lines.json / demo-score.json → ${outDir}`,
);
