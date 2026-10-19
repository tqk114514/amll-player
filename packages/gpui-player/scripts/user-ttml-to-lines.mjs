#!/usr/bin/env node
// 把用户的逐字级 TTML 转成浏览器 harness 消费的 AMLL lines JSON。
// 用法: node packages/gpui-player/scripts/user-ttml-to-lines.mjs <ttml 路径> <输出目录>
//
// 词结构对齐 GPUI 侧(ttml_processor + main.rs 的 ends_with_space 重建):
// 每个 span 一个词,span 之间的原始空格并入前一词的词尾。

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [input, outDir = "packages/gpui-player/testdata"] = process.argv.slice(2);
if (!input) {
	console.error("用法: user-ttml-to-lines.mjs <ttml> [输出目录]");
	process.exit(1);
}

const toMs = (stamp) => {
	const m = stamp.match(/(?:(\d+):)?(\d+):(\d+(?:\.\d+)?)/);
	return (Number(m[1] ?? 0) * 3600 + Number(m[2]) * 60 + Number(m[3])) * 1000;
};

const ttml = readFileSync(input, "utf8");
const lines = [];
for (const pMatch of ttml.matchAll(/<p begin="([^"]+)" end="([^"]+)"[^>]*>([\s\S]*?)<\/p>/g)) {
	const [, begin, end, body] = pMatch;
	const words = [];
	let pendingSpace = false;
	for (const node of body.matchAll(/<span begin="([^"]+)" end="([^"]+)"[^>]*>([^<]*)<\/span>|([^<]*)/g)) {
		if (node[1] !== undefined) {
			const text = node[3];
			if (text.trim().length === 0 && text.length > 0) {
				pendingSpace = true;
				continue;
			}
			words.push({
				word: pendingSpace ? ` ${text}` : text,
				startTime: toMs(node[1]),
				endTime: toMs(node[2]),
			});
			pendingSpace = false;
		} else {
			const raw = node[4] ?? "";
			if (raw.trim().length === 0 && raw.length > 0 && words.length > 0) {
				words.at(-1).word += " ";
			}
		}
	}
	lines.push({
		words,
		startTime: toMs(begin),
		endTime: toMs(end),
		translatedLyric: "",
		romanLyric: "",
		isBG: false,
		isDuet: false,
	});
}

const maxEnd = lines.reduce((acc, line) => Math.max(acc, line.endTime), 0);
mkdirSync(outDir, { recursive: true });
writeFileSync(join(outDir, "user-lines.json"), `${JSON.stringify(lines, null, "\t")}\n`, "utf8");
writeFileSync(
	join(outDir, "user-score.json"),
	`${JSON.stringify({ duration_ms: maxEnd + 2000, seeks: [] }, null, "\t")}\n`,
	"utf8",
);
console.log(`转换完成: ${lines.length} 行 → user-lines.json / user-score.json`);
