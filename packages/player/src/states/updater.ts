import { invoke } from "@tauri-apps/api/core";
import chalk from "chalk";
import { atom } from "jotai";
import { isCheckingUpdateAtom, updateInfoAtom } from "./appAtoms";

const LOG_TAG = chalk.bgHex("#FFAA00").hex("#FFFFFF")(" UPDATER ");

/** 后端 update_check 命令返回的检查结果 */
export interface UpdateInfo {
	currentVersion: string;
	version: string;
	body: string;
}

/** 后端 update_download_install 命令经 Channel 推送的下载事件 */
export interface DownloadEvent {
	event: "started" | "progress" | "finished";
	received: number;
	total: number;
}

export const checkUpdateAtom = atom(null, async (_get, set) => {
	set(isCheckingUpdateAtom, true);
	set(updateInfoAtom, false);
	try {
		const update = await invoke<UpdateInfo | null>("check_update");
		console.log(LOG_TAG, "检查更新返回结果", update);
		set(updateInfoAtom, update || false);
	} catch (e) {
		console.warn(LOG_TAG, "检查更新失败", e);
	} finally {
		set(isCheckingUpdateAtom, false);
	}
});
