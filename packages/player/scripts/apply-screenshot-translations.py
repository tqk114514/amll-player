# -*- coding: utf-8 -*-
# 为五个语言文件添加截图/录制页的 page.screenshot.* 词条
import json

ZH = {
    "page.screenshot.experimentalCallout": "本功能仅支持 Windows (WebView2) 且高度实验性",
    "page.screenshot.description": "本功能将会尝试通过 DevTools API 截取播放器窗口的画面内容，截图时请勿操作播放器，以免影响截图效果。",
    "page.screenshot.pixelUnit": "像素",
    "page.screenshot.takeScreenshotLoading": "正在截图，请稍候...",
    "page.screenshot.takeScreenshotSuccess": "截图成功！",
    "page.screenshot.takeScreenshotButton": "截图",
    "page.screenshot.copyImageButton": "复制图片",
    "page.screenshot.copyImageSuccess": "图片已复制到剪贴板！",
    "page.screenshot.copyImageFailed": "复制图片失败: {error}",
    "page.screenshot.copyImageWithShadowButton": "复制图片（带阴影）",
    "page.screenshot.canvasContextFailed": "无法获取画布上下文，请稍后再试。",
    "page.screenshot.loadImageFailed": "加载图片失败: {error}",
    "page.screenshot.elementNotFoundAfterRecord": "无法找到截图元素，请确保已成功录制。",
    "page.screenshot.elementNotFoundAfterLossless": "无法找到截图元素，请确保已成功截图。",
    "page.screenshot.record.resizeWindowLabel": "修改窗口大小",
    "page.screenshot.record.widthLabel": "录制宽度",
    "page.screenshot.record.heightLabel": "录制高度",
    "page.screenshot.record.recoverSizeLabel": "录制后恢复窗口大小",
    "page.screenshot.record.switchSource": "切换录制源",
    "page.screenshot.lossless.resizeWindowLabel": "修改窗口大小",
    "page.screenshot.lossless.widthLabel": "截图宽度",
    "page.screenshot.lossless.heightLabel": "截图高度",
    "page.screenshot.lossless.recoverSizeLabel": "截图后恢复窗口大小",
}

EN = {
    "page.screenshot.experimentalCallout": "This feature only supports Windows (WebView2) and is highly experimental",
    "page.screenshot.description": "This feature will attempt to capture the player window's content through the DevTools API. Do not interact with the player during capture, as it may affect the result.",
    "page.screenshot.pixelUnit": "px",
    "page.screenshot.takeScreenshotLoading": "Taking screenshot, please wait...",
    "page.screenshot.takeScreenshotSuccess": "Screenshot taken!",
    "page.screenshot.takeScreenshotButton": "Screenshot",
    "page.screenshot.copyImageButton": "Copy image",
    "page.screenshot.copyImageSuccess": "Image copied to clipboard!",
    "page.screenshot.copyImageFailed": "Failed to copy image: {error}",
    "page.screenshot.copyImageWithShadowButton": "Copy image (with shadow)",
    "page.screenshot.canvasContextFailed": "Failed to get canvas context, please try again later.",
    "page.screenshot.loadImageFailed": "Failed to load image: {error}",
    "page.screenshot.elementNotFoundAfterRecord": "Screenshot element not found, please make sure the recording succeeded.",
    "page.screenshot.elementNotFoundAfterLossless": "Screenshot element not found, please make sure the screenshot succeeded.",
    "page.screenshot.record.resizeWindowLabel": "Resize window",
    "page.screenshot.record.widthLabel": "Recording width",
    "page.screenshot.record.heightLabel": "Recording height",
    "page.screenshot.record.recoverSizeLabel": "Restore window size after recording",
    "page.screenshot.record.switchSource": "Switch capture source",
    "page.screenshot.lossless.resizeWindowLabel": "Resize window",
    "page.screenshot.lossless.widthLabel": "Screenshot width",
    "page.screenshot.lossless.heightLabel": "Screenshot height",
    "page.screenshot.lossless.recoverSizeLabel": "Restore window size after screenshot",
}

JA = {
    "page.screenshot.experimentalCallout": "この機能は Windows (WebView2) のみサポートしており、高度に実験的です",
    "page.screenshot.description": "この機能は DevTools API 経由でプレイヤーウィンドウの内容をキャプチャしようとします。キャプチャ中はプレイヤーを操作しないでください。結果に影響します。",
    "page.screenshot.pixelUnit": "ピクセル",
    "page.screenshot.takeScreenshotLoading": "スクリーンショットを撮影中です。お待ちください...",
    "page.screenshot.takeScreenshotSuccess": "スクリーンショットに成功しました！",
    "page.screenshot.takeScreenshotButton": "スクリーンショット",
    "page.screenshot.copyImageButton": "画像をコピー",
    "page.screenshot.copyImageSuccess": "画像をクリップボードにコピーしました！",
    "page.screenshot.copyImageFailed": "画像のコピーに失敗しました: {error}",
    "page.screenshot.copyImageWithShadowButton": "影付きで画像をコピー",
    "page.screenshot.canvasContextFailed": "キャンバスコンテキストを取得できませんでした。しばらくしてからもう一度お試しください。",
    "page.screenshot.loadImageFailed": "画像の読み込みに失敗しました: {error}",
    "page.screenshot.elementNotFoundAfterRecord": "スクリーンショット要素が見つかりません。録画が成功していることを確認してください。",
    "page.screenshot.elementNotFoundAfterLossless": "スクリーンショット要素が見つかりません。スクリーンショットが成功していることを確認してください。",
    "page.screenshot.record.resizeWindowLabel": "ウィンドウサイズを変更",
    "page.screenshot.record.widthLabel": "録画の幅",
    "page.screenshot.record.heightLabel": "録画の高さ",
    "page.screenshot.record.recoverSizeLabel": "録画後にウィンドウサイズを復元",
    "page.screenshot.record.switchSource": "キャプチャソースを切り替え",
    "page.screenshot.lossless.resizeWindowLabel": "ウィンドウサイズを変更",
    "page.screenshot.lossless.widthLabel": "スクリーンショットの幅",
    "page.screenshot.lossless.heightLabel": "スクリーンショットの高さ",
    "page.screenshot.lossless.recoverSizeLabel": "スクリーンショット後にウィンドウサイズを復元",
}

TW = {
    "page.screenshot.experimentalCallout": "本功能僅支援 Windows (WebView2) 且高度實驗性",
    "page.screenshot.description": "本功能將會嘗試透過 DevTools API 擷取播放器視窗的畫面內容，截圖時請勿操作播放器，以免影響截圖效果。",
    "page.screenshot.pixelUnit": "像素",
    "page.screenshot.takeScreenshotLoading": "正在截圖，請稍候...",
    "page.screenshot.takeScreenshotSuccess": "截圖成功！",
    "page.screenshot.takeScreenshotButton": "截圖",
    "page.screenshot.copyImageButton": "複製圖片",
    "page.screenshot.copyImageSuccess": "圖片已複製到剪貼簿！",
    "page.screenshot.copyImageFailed": "複製圖片失敗: {error}",
    "page.screenshot.copyImageWithShadowButton": "複製圖片（含陰影）",
    "page.screenshot.canvasContextFailed": "無法取得畫布上下文，請稍後再試。",
    "page.screenshot.loadImageFailed": "載入圖片失敗: {error}",
    "page.screenshot.elementNotFoundAfterRecord": "找不到截圖元素，請確認已成功錄製。",
    "page.screenshot.elementNotFoundAfterLossless": "找不到截圖元素，請確認已成功截圖。",
    "page.screenshot.record.resizeWindowLabel": "修改視窗大小",
    "page.screenshot.record.widthLabel": "錄製寬度",
    "page.screenshot.record.heightLabel": "錄製高度",
    "page.screenshot.record.recoverSizeLabel": "錄製後恢復視窗大小",
    "page.screenshot.record.switchSource": "切換錄製來源",
    "page.screenshot.lossless.resizeWindowLabel": "修改視窗大小",
    "page.screenshot.lossless.widthLabel": "截圖寬度",
    "page.screenshot.lossless.heightLabel": "截圖高度",
    "page.screenshot.lossless.recoverSizeLabel": "截圖後恢復視窗大小",
}

VI = {
    "page.screenshot.experimentalCallout": "Tính năng này chỉ hỗ trợ Windows (WebView2) và đang ở giai đoạn thử nghiệm cao",
    "page.screenshot.description": "Tính năng này sẽ cố gắng chụp nội dung cửa sổ trình phát thông qua DevTools API. Đừng thao tác với trình phát khi chụp以免 ảnh hưởng đến kết quả.",
    "page.screenshot.pixelUnit": "điểm ảnh",
    "page.screenshot.takeScreenshotLoading": "Đang chụp màn hình, vui lòng chờ...",
    "page.screenshot.takeScreenshotSuccess": "Chụp màn hình thành công!",
    "page.screenshot.takeScreenshotButton": "Chụp màn hình",
    "page.screenshot.copyImageButton": "Sao chép ảnh",
    "page.screenshot.copyImageSuccess": "Đã sao chép ảnh vào bộ nhớ tạm!",
    "page.screenshot.copyImageFailed": "Sao chép ảnh thất bại: {error}",
    "page.screenshot.copyImageWithShadowButton": "Sao chép ảnh (kèm bóng đổ)",
    "page.screenshot.canvasContextFailed": "Không thể lấy ngữ cảnh canvas, vui lòng thử lại sau.",
    "page.screenshot.loadImageFailed": "Tải ảnh thất bại: {error}",
    "page.screenshot.elementNotFoundAfterRecord": "Không tìm thấy phần tử chụp màn hình, hãy đảm bảo việc ghi hình đã thành công.",
    "page.screenshot.elementNotFoundAfterLossless": "Không tìm thấy phần tử chụp màn hình, hãy đảm bảo việc chụp đã thành công.",
    "page.screenshot.record.resizeWindowLabel": "Đổi kích thước cửa sổ",
    "page.screenshot.record.widthLabel": "Chiều rộng ghi hình",
    "page.screenshot.record.heightLabel": "Chiều cao ghi hình",
    "page.screenshot.record.recoverSizeLabel": "Khôi phục kích thước cửa sổ sau khi ghi",
    "page.screenshot.record.switchSource": "Chuyển nguồn ghi hình",
    "page.screenshot.lossless.resizeWindowLabel": "Đổi kích thước cửa sổ",
    "page.screenshot.lossless.widthLabel": "Chiều rộng chụp màn hình",
    "page.screenshot.lossless.heightLabel": "Chiều cao chụp màn hình",
    "page.screenshot.lossless.recoverSizeLabel": "Khôi phục kích thước cửa sổ sau khi chụp",
}

ALL = {"zh-CN": ZH, "en-US": EN, "ja-JP": JA, "zh-TW": TW, "vi-VN": VI}

for locale, translations in ALL.items():
    path = f"locales/{locale}/translation.json"
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    for key, value in translations.items():
        node = data
        parts = key.split(".")
        for part in parts[:-1]:
            node = node.setdefault(part, {})
        node[parts[-1]] = value
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent="\t")
        f.write("\n")
    print(f"applied {len(translations)} keys to {locale}")
