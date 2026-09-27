const $ = selector => document.querySelector(selector);
let release;
let loading = false;

function selectPlatform() {
	const platform = release.platforms.find(item => item.id === $("#install-platform").value);
	const download = $("#download-link");
	download.classList.toggle("disabled", !platform);
	download.setAttribute("aria-disabled", String(!platform));
	if (platform) download.href = platform.url;
	else download.removeAttribute("href");
	$("#install-command").textContent = platform?.command ?? "";
	$("#install-command-box").classList.toggle("hidden", !platform?.command);
	$("#command-label").textContent = platform?.id === "windows" ? "在 PowerShell 中执行" : "在终端执行";
	$("#copy-install").textContent = "复制命令";
	$("#install-status").textContent = platform?.command ? "执行命令即可下载并保存连接配置，随后按终端提示启动采集。"
		: "安装命令暂不可用，请确认 API 部署已完成。";
}

async function loadRelease() {
	if (loading) return;
	loading = true;
	$("#install-retry").classList.add("hidden");
	$("#install-status").textContent = "正在读取最新版本…";
	try {
		const response = await fetch("/api/releases", { cache: "no-store" });
		if (!response.ok) throw new Error(`加载失败（${response.status}）`);
		release = await response.json();
		const select = $("#install-platform");
		select.replaceChildren(...release.platforms.map(platform => new Option(platform.label, platform.id)));
		select.disabled = !release.platforms.length;
		const preferred = /Win/i.test(navigator.platform) ? "windows" : /Mac/i.test(navigator.platform) ? "macos-arm64" : "linux";
		if (release.platforms.some(item => item.id === preferred)) select.value = preferred;
		$("#release-link").href = `https://github.com/HSwift/tokscale-serverless/releases/tag/${encodeURIComponent(release.tag)}`;
		$("#release-link").textContent = `${release.tag} ↗`;
		selectPlatform();
		$("#install-retry").classList.toggle("hidden", release.platforms.some(platform => platform.command));
	} catch (error) {
		$("#install-status").textContent = error.message;
		$("#install-retry").classList.remove("hidden");
	} finally { loading = false; }
}

export function initInstaller() {
	const dialog = $("#install-dialog");
	$("#install-open").addEventListener("click", () => { dialog.showModal(); void loadRelease(); });
	$("#install-close").addEventListener("click", () => dialog.close());
	dialog.addEventListener("click", event => {
		const rect = dialog.getBoundingClientRect();
		if (event.target === dialog && (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom)) dialog.close();
	});
	$("#install-platform").addEventListener("change", selectPlatform);
	$("#install-retry").addEventListener("click", loadRelease);
	$("#copy-install").addEventListener("click", async () => {
		try {
			await navigator.clipboard.writeText($("#install-command").textContent);
			$("#copy-install").textContent = "已复制";
		} catch { $("#install-status").textContent = "请选中上方命令手动复制。"; }
	});
}
