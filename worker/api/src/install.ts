import type { Env } from "./auth";

const REPOSITORY = "HSwift/tokscale-serverless";
const DOWNLOAD_BASE = `https://github.com/${REPOSITORY}/releases/download/`;
const PLATFORMS = [
	{ id: "linux", label: "Linux · x86_64", target: "x86_64-unknown-linux-musl", extension: "tar.gz" },
	{ id: "macos-arm64", label: "macOS · Apple Silicon", target: "aarch64-apple-darwin", extension: "tar.gz" },
	{ id: "macos-x64", label: "macOS · Intel", target: "x86_64-apple-darwin", extension: "tar.gz" },
	{ id: "windows", label: "Windows · x86_64", target: "x86_64-pc-windows-msvc", extension: "zip" },
] as const;
type Platform = typeof PLATFORMS[number];

function response(body: unknown, status = 200): Response {
	return Response.json(body, { status, headers: { "cache-control": "no-store", "referrer-policy": "no-referrer" } });
}

async function latestRelease() {
	const result = await fetch(`https://api.github.com/repos/${REPOSITORY}/releases/latest`, {
		headers: { Accept: "application/vnd.github+json", "User-Agent": "tokscale-serverless" },
		cf: { cacheTtl: 300, cacheEverything: true },
		signal: AbortSignal.timeout(10_000),
	});
	if (!result.ok) throw new Error("Release lookup failed");
	const data = await result.json<{ tag_name: string; draft: boolean; prerelease: boolean; assets: { name: string }[] }>();
	if (data.draft || data.prerelease || typeof data.tag_name !== "string" || !/^[\w][\w./-]{0,100}$/.test(data.tag_name) || !Array.isArray(data.assets)) {
		throw new Error("Invalid release");
	}
	const names = new Set(data.assets.map(asset => asset.name));
	const platforms = PLATFORMS.filter(platform => names.has(assetName(platform))).map(platform => ({
		id: platform.id, label: platform.label, asset: assetName(platform),
		url: `${DOWNLOAD_BASE}${encodeURIComponent(data.tag_name)}/${assetName(platform)}`,
	}));
	return { tag: data.tag_name, platforms };
}

function assetName(platform: Platform): string {
	return `tokscale-client-${platform.target}.${platform.extension}`;
}

export async function releases(env: Env): Promise<Response> {
	try {
		const [release, deployment] = await Promise.all([
			latestRelease(), env.DB.prepare("SELECT origin FROM api_origin WHERE id = 1").first<{ origin: string }>(),
		]);
		const apiUrl = deployment?.origin;
		return response({ tag: release.tag, platforms: release.platforms.map(platform => ({
			...platform, command: apiUrl && env.INGEST_TOKEN ? installCommand(platform.id, release.tag, apiUrl, env.INGEST_TOKEN) : null,
		})) });
	} catch { return response({ error: { message: "暂时无法读取最新版本，请稍后重试。" } }, 502); }
}

export function installCommand(platform: string, tag: string, origin: string, token: string): string {
	const params = new URLSearchParams({ platform, tag });
	if (platform === "windows") {
		return `$env:SYNC_URL=${psQuote(origin)}; $env:SYNC_TOKEN=${psQuote(token)}; irm ${psQuote(`${origin}/install.ps1?${params}`)} | iex`;
	}
	return `curl -fsSL ${shellQuote(`${origin}/install.sh?${params}`)} | SYNC_URL=${shellQuote(origin)} SYNC_TOKEN=${shellQuote(token)} bash`;
}

// Public, reusable script. Credentials come from the command's environment.
export function installerScript(url: URL): Response {
	const platform = PLATFORMS.find(item => item.id === url.searchParams.get("platform"));
	const tag = url.searchParams.get("tag") ?? "";
	if (!platform || !/^[\w][\w./-]{0,100}$/.test(tag) ||
		(url.pathname === "/install.ps1") !== (platform.id === "windows")) {
		return response({ error: { message: "Invalid platform or release tag" } }, 400);
	}
	return new Response(renderInstaller(platform, tag), { headers: { "content-type": "text/plain; charset=utf-8" } });
}

const shellQuote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
const psQuote = (value: string) => `'${value.replaceAll("'", "''")}'`;

export function renderInstaller(platform: Platform, tag: string): string {
	const asset = assetName(platform);
	const base = `${DOWNLOAD_BASE}${encodeURIComponent(tag)}/`;
	if (platform.id === "windows") return renderPowerShell(base, asset);
	const os = platform.id === "linux" ? "Linux" : "Darwin";
	const arch = platform.id === "macos-arm64" ? "arm64" : "x86_64";
	return `#!/usr/bin/env bash
set +x
set -euo pipefail
if [ "$(uname -s)" != ${shellQuote(os)} ] || [ "$(uname -m)" != ${shellQuote(arch)} ]; then
  echo 'This installer does not match your OS/architecture. Select the correct download in the console.' >&2
  exit 1
fi
for tool in curl tar; do command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }; done
install_tmp=$(mktemp -d)
trap 'rm -rf "$install_tmp"' EXIT
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 ${shellQuote(base + asset)} -o "$install_tmp/${asset}"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 ${shellQuote(base + "SHA256SUMS")} -o "$install_tmp/SHA256SUMS"
expected=$(awk -v file=${shellQuote(asset)} '$2 == file { print $1 }' "$install_tmp/SHA256SUMS")
[ "\${#expected}" -eq 64 ] || { echo 'Missing checksum.' >&2; exit 1; }
if command -v sha256sum >/dev/null; then
  actual=$(sha256sum "$install_tmp/${asset}" | awk '{print $1}')
else
  actual=$(shasum -a 256 "$install_tmp/${asset}" | awk '{print $1}')
fi
[ "$expected" = "$actual" ] || { echo 'Checksum verification failed.' >&2; exit 1; }
tar -xzf "$install_tmp/${asset}" -C "$install_tmp" tokscale-client
chmod 755 "$install_tmp/tokscale-client"
# The existing collector verifies and persists credentials, preserving device identity.
"$install_tmp/tokscale-client" connect
mkdir -p "$HOME/.local/bin"
install -m 755 "$install_tmp/tokscale-client" "$HOME/.local/bin/tokscale-client"
echo 'Installed and connected. Start collecting with:'
echo '  "$HOME/.local/bin/tokscale-client" run'
`;
}

function renderPowerShell(base: string, asset: string): string {
	return `# Tokscale collector installer
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT' -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64' -or -not [Environment]::Is64BitOperatingSystem) { throw 'Select the installer for your OS/architecture.' }
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$installTemp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
try {
  New-Item -ItemType Directory -Path $installTemp | Out-Null
  $archive = Join-Path $installTemp ${psQuote(asset)}
  Invoke-WebRequest -UseBasicParsing -Uri ${psQuote(base + asset)} -OutFile $archive
  $checksumPath = Join-Path $installTemp 'SHA256SUMS'
  Invoke-WebRequest -UseBasicParsing -Uri ${psQuote(base + "SHA256SUMS")} -OutFile $checksumPath
  $checksums = Get-Content -LiteralPath $checksumPath -Raw
  $expected = ($checksums -split '\\r?\\n' | Where-Object { ($_ -split '\\s+')[1] -eq ${psQuote(asset)} } | ForEach-Object { ($_ -split '\\s+')[0] })
  if (-not $expected -or $expected -notmatch '^[a-fA-F0-9]{64}$' -or (Get-FileHash $archive -Algorithm SHA256).Hash -ne $expected) { throw 'Checksum verification failed.' }
  Expand-Archive -Path $archive -DestinationPath $installTemp
  $binary = Join-Path $installTemp 'tokscale-client.exe'
  & $binary connect
  if ($LASTEXITCODE -ne 0) { throw 'Connection verification failed.' }
  $destination = Join-Path $env:LOCALAPPDATA 'Programs\\tokscale'
  New-Item -ItemType Directory -Force -Path $destination | Out-Null
  Copy-Item $binary (Join-Path $destination 'tokscale-client.exe') -Force
  Write-Host 'Installed and connected. Start collecting with:'
  Write-Host ('  & "' + (Join-Path $destination 'tokscale-client.exe') + '" run')
} finally {
  if (Test-Path $installTemp) { Remove-Item $installTemp -Recurse -Force }
}
`;
}
