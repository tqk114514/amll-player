#!/usr/bin/env node
// 数值对比 harness 的本地服务器:
//   GET  /...            —— 以仓库根为根的静态文件(供页面加载 core bundle 与测试数据)
//   POST /save/<name>    —— 接收页面采样的 JSONL,落盘到 captures/<name>
// 用法: node packages/gpui-player/scripts/trace-harness/serve.mjs [端口]

import { createServer } from "node:http";
import { createReadStream, existsSync, mkdirSync, statSync, writeFileSync } from "node:fs";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = resolve(dirname(fileURLToPath(import.meta.url)));
const repoRoot = resolve(here, "..", "..", "..", "..");
const capturesDir = join(here, "captures");
const port = Number(process.argv[2] ?? 39871);

const MIME = {
	".html": "text/html; charset=utf-8",
	".js": "text/javascript; charset=utf-8",
	".mjs": "text/javascript; charset=utf-8",
	".json": "application/json; charset=utf-8",
	".css": "text/css; charset=utf-8",
	".svg": "image/svg+xml",
};

mkdirSync(capturesDir, { recursive: true });

createServer((req, res) => {
	const url = new URL(req.url ?? "/", `http://localhost:${port}`);
	if (req.method === "POST" && url.pathname.startsWith("/save/")) {
		const name = url.pathname.replace("/save/", "").replace(/[^a-zA-Z0-9._-]/g, "_");
		const chunks = [];
		req.on("data", (chunk) => chunks.push(chunk));
		req.on("end", () => {
			const file = join(capturesDir, name);
			writeFileSync(file, Buffer.concat(chunks));
			console.log(`已保存: ${file} (${Buffer.concat(chunks).length} 字节)`);
			res.writeHead(200, { "content-type": "text/plain" });
			res.end("ok");
		});
		return;
	}
	if (req.method !== "GET") {
		res.writeHead(405).end();
		return;
	}
	const rel = normalize(decodeURIComponent(url.pathname)).replace(/^([/\\])+/, "");
	const file = join(repoRoot, rel);
	if (!file.startsWith(repoRoot) || !existsSync(file) || statSync(file).isDirectory()) {
		res.writeHead(404).end("not found");
		return;
	}
	res.writeHead(200, { "content-type": MIME[extname(file)] ?? "application/octet-stream" });
	createReadStream(file).pipe(res);
}).listen(port, "127.0.0.1", () => {
	console.log(`harness 服务: http://127.0.0.1:${port}/packages/gpui-player/scripts/trace-harness/index.html`);
	console.log(`仓库根: ${repoRoot}`);
});

function dirname(path) {
	const idx = path.replace(/\\/g, "/").lastIndexOf("/");
	return idx === -1 ? "." : path.slice(0, idx);
}
