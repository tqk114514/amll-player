// 从 @applemusic-like-lyrics/core 的产物中提取真实的 Spring 实现,
// 用与 Rust 移植完全相同的操作序列跑出逐帧位置,导出为数值验收 fixture。
// 用法: 在 packages/gpui-player 下执行 node scripts/gen-spring-fixture.mjs
import { readFileSync, writeFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const bundlePath = resolve(
	here,
	"../../player/node_modules/@applemusic-like-lyrics/core/dist/amll-core.mjs",
);
const bundle = readFileSync(bundlePath, "utf8");

// 按 region 标记截取 derivative.ts + spring.ts 两段(与上游源文件一一对应)
const startMarker = "//#region src/utils/derivative.ts";
const endMarker = "//#region src/lyric-player/dom/bottom-line.ts";
const start = bundle.indexOf(startMarker);
const end = bundle.indexOf(endMarker);
if (start < 0 || end < 0 || end <= start) {
	throw new Error("未能在产物中定位 spring 源码区域");
}
const extracted = bundle
	.slice(start, end)
	// 弹簧区域不需要 __exportAll 的导出登记
	.split("\n")
	.filter((line) => !line.includes("__exportAll"))
	.join("\n");

// Duration 在产物里是毫秒垫片(asSecsF64 = d/1e3),等价替换
const prelude = "const Duration = { ZERO: 0, asSecsF64: (d) => d / 1e3 };\n";

const repeat = (n, op) => Array.from({ length: n }, () => ({ ...op }));
// 场景以毫秒描述,与上游 API 一致
const scenarios = [
	{
		name: "basic_settle",
		ops: [{ t: "target", position: 100, delay: 0 }, ...repeat(120, { t: "update", dt: 16.667 })],
	},
	{
		name: "midflight_switch",
		ops: [
			{ t: "target", position: 100, delay: 0 },
			...repeat(30, { t: "update", dt: 16.667 }),
			{ t: "target", position: 200, delay: 0 },
			...repeat(90, { t: "update", dt: 16.667 }),
		],
	},
	{
		name: "delayed_target",
		ops: [
			{ t: "target", position: 100, delay: 0 },
			...repeat(30, { t: "update", dt: 16.667 }),
			{ t: "target", position: 200, delay: 50 },
			...repeat(120, { t: "update", dt: 16.667 }),
		],
	},
	{
		name: "params_change",
		ops: [
			{ t: "target", position: 100, delay: 0 },
			...repeat(30, { t: "update", dt: 16.667 }),
			{ t: "params", stiffness: 140, damping: 22, delay: 0 },
			...repeat(90, { t: "update", dt: 16.667 }),
		],
	},
	{
		name: "slow_policy_seek",
		ops: [
			{ t: "params", stiffness: 90, damping: 15, mass: 0.9, delay: 0 },
			{ t: "target", position: 800, delay: 0 },
			...repeat(15, { t: "update", dt: 16.667 }),
			{ t: "target", position: 80, delay: 0 },
			...repeat(150, { t: "update", dt: 16.667 }),
		],
	},
	{
		name: "interval_policy",
		ops: [
			{ t: "params", stiffness: 196.40556, damping: 30.826, delay: 0 },
			{ t: "target", position: 1000, delay: 0 },
			...repeat(120, { t: "update", dt: 16.667 }),
		],
	},
	{
		name: "queue_replaced_by_immediate",
		ops: [
			{ t: "target", position: 100, delay: 0 },
			...repeat(10, { t: "update", dt: 16.667 }),
			{ t: "target", position: 200, delay: 100 },
			...repeat(3, { t: "update", dt: 16.667 }),
			{ t: "target", position: 300, delay: 0 },
			...repeat(90, { t: "update", dt: 16.667 }),
		],
	},
	{
		name: "delayed_params_during_flight",
		ops: [
			{ t: "target", position: 500, delay: 0 },
			...repeat(20, { t: "update", dt: 16.667 }),
			{ t: "params", stiffness: 90, damping: 15, delay: 60 },
			...repeat(130, { t: "update", dt: 16.667 }),
		],
	},
];

// 在与上游相同的 API(毫秒)上执行场景,收集每帧位置;
// 同时打补丁记录每次求解器重置的时刻/位置/初速度,便于定位移植差异
const driver = `
const scenarios = ${JSON.stringify(scenarios)};
const origReset = Spring.prototype.resetSolver;
Spring.prototype.resetSolver = function () {
	const v0 = this.getV(this.currentTime);
	const t = this.currentTime;
	const from = this.currentPosition;
	origReset.call(this);
	this.__resets.push({ t, from, v0, to: this.targetPosition });
};
const results = [];
for (const scenario of scenarios) {
	const spring = new Spring(0);
	spring.__resets = [];
	const frames = [];
	for (const op of scenario.ops) {
		if (op.t === "target") spring.setTargetPosition(op.position, op.delay);
		else if (op.t === "params") {
			const patch = {};
			if (op.stiffness !== undefined) patch.stiffness = op.stiffness;
			if (op.damping !== undefined) patch.damping = op.damping;
			if (op.mass !== undefined) patch.mass = op.mass;
			if (op.soft !== undefined) patch.soft = op.soft;
			spring.updateParams(patch, op.delay ?? 0);
		} else if (op.t === "update") {
			spring.update(op.dt);
			frames.push(spring.getCurrentPosition());
		}
	}
	results.push({ name: scenario.name, ops: scenario.ops, frames, resets: spring.__resets });
}
return JSON.stringify({ note: "AMLL core Spring 逐帧 fixture,时长单位 ms", results });
`;

// 提取源码来自受控的 node_modules,无不可信输入
const moduleSource = prelude + extracted + "\n" + driver;
const fn = new Function(moduleSource);
const fixtureJson = fn();
const parsed = JSON.parse(fixtureJson);
const totalFrames = parsed.results.reduce((sum, s) => sum + s.frames.length, 0);

const outPath = resolve(here, "../src/testdata/spring-fixture.json");
writeFileSync(outPath, JSON.stringify(parsed, null, "\t") + "\n", "utf8");
console.log(`fixture 已生成: ${outPath}(${parsed.results.length} 场景 / ${totalFrames} 帧)`);
