#!/usr/bin/env node
// 打包采集 harness 的自包含 core bundle:
//   node packages/gpui-player/scripts/trace-harness/build-core.mjs
// 用仓库自带的 rolldown(Vite 8 的绑定器),无需安装新依赖。

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "..", "..", "..", "..");
const pnpmDir = join(repoRoot, "node_modules", ".pnpm");

const rolldownDir = readdirSync(pnpmDir)
	.find((entry) => /^rolldown@\d/.test(entry))
	?? (() => {
		throw new Error("pnpm store 里没有找到 rolldown");
	})();
const cli = join(pnpmDir, rolldownDir, "node_modules", "rolldown", "dist", "cli.mjs");
if (!existsSync(cli)) throw new Error(`找不到 rolldown CLI: ${cli}`);

const input = join(here, "entry.mjs");
const output = join(here, "harness-core.mjs");
execFileSync(process.execPath, [cli, "--input", input, "--format", "esm", "--file", output], {
	cwd: repoRoot,
	stdio: "inherit",
});
console.log(`已生成: ${output}`);
