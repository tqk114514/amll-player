import {
	Flex,
	Text,
	TextField,
	Switch,
	Button,
	Callout,
} from "@radix-ui/themes";
import { useAtom } from "jotai";
import { useRef, useState } from "react";
import { toast } from "react-toastify";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
	resizeWindowAtom,
	targetWidthAtom,
	targetHeightAtom,
	recoverWindowSizeAtom,
	recordMediaStreamAtom,
} from "./states.ts";

const RecordTab = () => {
	const { t } = useTranslation();
	const [resizeWindow, setResizeWindow] = useAtom(resizeWindowAtom);
	const [targetWidth, setTargetWidth] = useAtom(targetWidthAtom);
	const [targetHeight, setTargetHeight] = useAtom(targetHeightAtom);
	const [recoverWindowSize, setRecoverWindowSize] = useAtom(
		recoverWindowSizeAtom,
	);
	const [, setRecordMediaStream] = useAtom(recordMediaStreamAtom);
	const [screenshotData, setScreenshotData] = useState<string | undefined>(
		undefined,
	);
	const previewRef = useRef<HTMLVideoElement>(null);

	return (
		<Flex direction="column" pt="2" gap="2" height="100%" overflow="hidden">
			<Callout.Root color="orange">
				{t("page.screenshot.experimentalCallout")}
			</Callout.Root>
			<Text as="p">
				{t("page.screenshot.description")}
			</Text>
			<Text as="label">
				<Flex align="center" justify="between" gap="2">
					<Text as="span">
						{t("page.screenshot.record.resizeWindowLabel")}
					</Text>
					<Switch checked={resizeWindow} onCheckedChange={setResizeWindow} />
				</Flex>
			</Text>
			<Text as="label">
				<Flex align="center" justify="between" gap="2">
					<Text as="span">
						{t("page.screenshot.record.widthLabel")}
					</Text>
					<TextField.Root
						name="screenshot.record.width"
						disabled={!resizeWindow}
						type="number"
						value={targetWidth}
						onChange={(v) => setTargetWidth(v.target.valueAsNumber)}
						min={0}
						max={65536}
						step={1}
					>
						<TextField.Slot />
						<TextField.Slot>
							{t("page.screenshot.pixelUnit")}
						</TextField.Slot>
					</TextField.Root>
				</Flex>
			</Text>
			<Text as="label">
				<Flex align="center" justify="between" gap="2">
					<Text as="span">
						{t("page.screenshot.record.heightLabel")}
					</Text>
					<TextField.Root
						name="screenshot.record.height"
						disabled={!resizeWindow}
						type="number"
						value={targetHeight}
						onChange={(v) => setTargetHeight(v.target.valueAsNumber)}
						min={0}
						max={65536}
						step={1}
					>
						<TextField.Slot />
						<TextField.Slot>
							{t("page.screenshot.pixelUnit")}
						</TextField.Slot>
					</TextField.Root>
				</Flex>
			</Text>
			<Text as="label">
				<Flex align="center" justify="between" gap="2">
					<Text as="span">
						{t("page.screenshot.record.recoverSizeLabel")}
					</Text>
					<Switch
						disabled={!resizeWindow}
						checked={recoverWindowSize}
						onCheckedChange={setRecoverWindowSize}
					/>
				</Flex>
			</Text>
			<Flex gap="2" justify="center">
				<Button
					onClick={async () => {
						const media = await navigator.mediaDevices.getDisplayMedia({
							video: {
								displaySurface: "browser",
								frameRate: {
									ideal: 60,
									max: 144,
								},
							},
						});

						const preview = previewRef.current;
						if (preview) {
							preview.srcObject = media;
							preview.play();
						}

						console.log("获取到的媒体流：", media);

						setRecordMediaStream((old) => {
							if (old) {
								for (const track of old.getTracks()) {
									track.stop();
								}
							}

							return media;
						});
					}}
				>
					{t("page.screenshot.record.switchSource")}
				</Button>
				<Button
					onClick={async () => {
						const p = toast.loading(
							t("page.screenshot.takeScreenshotLoading"),
						);
						const data = (await invoke("take_screenshot", {
							targetWidth,
							targetHeight,
							recoverSize: recoverWindowSize,
							resizeWindow,
						})) as string;
						const uri = `data:image/png;base64,${data}`;
						const img = new Image();
						img.src = uri;
						img.onload = () => {
							setTargetWidth(img.width);
							setTargetHeight(img.height);
							setScreenshotData(uri);

							toast.update(p, {
								render: t("page.screenshot.takeScreenshotSuccess"),
								type: "success",
								isLoading: false,
								autoClose: 2000,
								pauseOnFocusLoss: false,
								pauseOnHover: false,
							});
						};
					}}
				>
					{t("page.screenshot.takeScreenshotButton")}
				</Button>
			</Flex>
			<Flex gap="2" justify="center">
				<Button
					onClick={async () => {
						if (!screenshotData) {
							toast.error(
								t("page.screenshot.elementNotFoundAfterRecord"),
							);
							return;
						}
						const data = atob(screenshotData.slice(22));
						const byteArray = new Uint8Array(data.length);
						for (let i = 0; i < data.length; i++) {
							byteArray[i] = data.charCodeAt(i);
						}
						const blob = new Blob([byteArray], { type: "image/png" });
						navigator.clipboard
							.write([
								new ClipboardItem({
									"image/png": blob,
								}),
							])
							.then(() => {
								toast.success(
									t("page.screenshot.copyImageSuccess"),
									{
										autoClose: 2000,
										pauseOnFocusLoss: false,
										pauseOnHover: false,
									},
								);
							})
							.catch((err) => {
								toast.error(
									t("page.screenshot.copyImageFailed", {
										error: err.message,
									}),
								);
							});
						}}
					>
						{t("page.screenshot.copyImageButton")}
					</Button>
				<Button
					onClick={async () => {
						const svgEl = document.getElementById(
							"output",
						) as SVGSVGElement | null;
						if (!svgEl) {
							toast.error(t("page.screenshot.elementNotFoundAfterRecord"));
							return;
						}
						const canvas = document.createElement("canvas");
						canvas.width = targetWidth + 200;
						canvas.height = targetHeight + 200;
						const ctx = canvas.getContext("2d");
						if (!ctx) {
							toast.error(
								t("page.screenshot.canvasContextFailed"),
							);
							return;
						}
						const img = new Image();
						img.onload = () => {
							ctx.drawImage(img, 0, 0);
							canvas.toBlob((blob) => {
								navigator.clipboard
									.write([
										new ClipboardItem({
											"image/png": blob as Blob,
										}),
									])
									.then(() => {
										toast.success(t("page.screenshot.copyImageSuccess"), {
											autoClose: 2000,
											pauseOnFocusLoss: false,
											pauseOnHover: false,
										});
									})
									.catch((err) => {
										toast.error(
											t("page.screenshot.copyImageFailed", {
												error: err.message,
											}),
										);
									});
							});
						};
						img.onerror = (err) => {
							toast.error(
								t("page.screenshot.loadImageFailed", {
									error: String(err),
								}),
							);
						};
						const svgData = new XMLSerializer().serializeToString(svgEl);
						img.src = `data:image/svg+xml;base64,${btoa(svgData)}`;
					}}
				>
					{t("page.screenshot.copyImageWithShadowButton")}
				</Button>
			</Flex>
			<Flex align="center" justify="center" flexGrow="1" overflow="hidden">
				<video autoPlay muted ref={previewRef} />
			</Flex>
		</Flex>
	);
};

export default RecordTab;
