const $ = selector => document.querySelector(selector);
let release;
let requestVersion = 0;
let expiryTimer;

async function json(path, options) {
	const result = await fetch(path, options);
	let body;
	try { body = await result.json(); } catch { throw new Error("会话已过期，请刷新页面重新登录。"); }
	if (!result.ok) throw new Error(body?.error?.message ?? `HTTP ${result.status}`);
	return body;
}

function resetCommand() {
	requestVersion++;
	clearTimeout(expiryTimer);
	$("#install-command").textContent = "";
	$("#install-command-box").classList.add("hidden");
	$("#copy-install").textContent = "复制命令";
}

function selectPlatform() {
	resetCommand();
	const platform = release?.platforms.find(item => item.id === $("#install-platform").value);
	const download = $("#download-link");
	download.classList.toggle("disabled", !platform);
	download.setAttribute("aria-disabled", String(!platform));
	if (platform) download.href = platform.url;
	else download.removeAttribute("href");
	$("#install-generate").disabled = !platform || !release.installReady;
	$("#install-status").textContent = !platform ? "此版本暂未提供可下载的安装包。"
		: release.installReady ? `${release.tag} · 一键安装会校验下载文件，并自动保存当前服务的连接配置。`
			: !release.apiUrl ? `${release.tag} · 等待采集器完成一次同步以识别连接地址；首次部署请先手动连接一台采集器。`
			: `${release.tag} · 可直接下载。自动配置安装暂不可用，请联系管理员。`;
	$("#install-retry").classList.toggle("hidden", !!release.installReady);
}

async function loadRelease() {
	resetCommand();
	$("#install-retry").classList.add("hidden");
	$("#install-status").textContent = "正在读取最新正式版本…";
	try {
		release = await json("/api/releases");
		const select = $("#install-platform");
		select.replaceChildren(...release.platforms.map(platform => new Option(platform.label, platform.id)));
		select.disabled = !release.platforms.length;
		const preferred = /Win/i.test(navigator.platform) ? "windows" : /Mac/i.test(navigator.platform) ? "macos-arm64" : "linux";
		if (release.platforms.some(item => item.id === preferred)) select.value = preferred;
		$("#release-link").href = `https://github.com/HSwift/tokscale-serverless/releases/tag/${encodeURIComponent(release.tag)}`;
		$("#release-link").textContent = `${release.tag} · 发布说明 ↗`;
		selectPlatform();
	} catch (error) {
		$("#install-status").textContent = error.message;
		$("#install-retry").classList.remove("hidden");
	}
}

export function initInstaller() {
	$("#install-platform").addEventListener("change", selectPlatform);
	$("#install-retry").addEventListener("click", loadRelease);
	$("#install-generate").addEventListener("click", async () => {
		resetCommand();
		const version = requestVersion;
		const platform = $("#install-platform").value;
		$("#install-generate").disabled = true;
		$("#install-status").textContent = "正在生成专属安装命令…";
		try {
			const result = await json("/api/install-tickets", {
				method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ platform }),
			});
			if (version !== requestVersion) return;
			$("#install-command").textContent = result.command;
			$("#command-label").textContent = platform === "windows" ? "在目标电脑的 PowerShell 中执行" : "在目标电脑的终端中执行";
			$("#install-command-box").classList.remove("hidden");
			$("#install-status").textContent = `${result.tag} · 命令 10 分钟内有效，仅可使用一次。安装后按终端提示启动采集。`;
			expiryTimer = setTimeout(() => {
				resetCommand();
				$("#install-status").textContent = "安装命令已过期，请重新生成。";
			}, Math.max(0, result.expiresAt - Date.now()));
		} catch (error) {
			if (version === requestVersion) $("#install-status").textContent = error.message;
		} finally {
			if (version === requestVersion) $("#install-generate").disabled = false;
		}
	});
	$("#copy-install").addEventListener("click", async () => {
		try {
			await navigator.clipboard.writeText($("#install-command").textContent);
			$("#copy-install").textContent = "已复制";
		} catch { $("#install-status").textContent = "无法自动复制，请选中上方命令手动复制。"; }
	});
	void loadRelease();
}
