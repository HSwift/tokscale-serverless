import { secureEqual, type Env } from "./auth";

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
		const release = await latestRelease();
		return response({ tag: release.tag, platforms: release.platforms.map(platform => ({
			...platform, installPath: env.INGEST_TOKEN ? installationPath(platform.id, release.tag, env.INGEST_TOKEN) : null,
		})) });
	} catch { return response({ error: { message: "暂时无法读取最新版本，请稍后重试。" } }, 502); }
}

function installationPath(platform: string, version: string, token: string): string {
	const params = new URLSearchParams({ token, type: platform, version });
	return `/install.${platform === "windows" ? "ps1" : "sh"}?${params}`;
}

// Authenticate before returning a script containing the connection settings.
export function installerScript(url: URL, env: Env): Response {
	if (!env.INGEST_TOKEN) return response({ error: { message: "INGEST_TOKEN is not configured" } }, 503);
	const token = url.searchParams.get("token") ?? "";
	if (!secureEqual(token, env.INGEST_TOKEN)) {
		return response({ error: { message: "Invalid installation token" } }, 401);
	}
	const platform = PLATFORMS.find(item => item.id === url.searchParams.get("type"));
	const tag = url.searchParams.get("version") ?? "";
	if (!platform || !/^[\w][\w./-]{0,100}$/.test(tag) ||
		(url.pathname === "/install.ps1") !== (platform.id === "windows")) {
		return response({ error: { message: "Invalid platform or release tag" } }, 400);
	}
	return new Response(renderInstaller(platform, tag, url.origin, env.INGEST_TOKEN), {
		headers: {
			"content-type": "text/plain; charset=utf-8", "cache-control": "no-store",
			"referrer-policy": "no-referrer", "x-content-type-options": "nosniff",
		},
	});
}

const shellQuote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
const psQuote = (value: string) => `'${value.replaceAll("'", "''")}'`;

function renderInstaller(platform: Platform, tag: string, origin: string, token: string): string {
	const asset = assetName(platform);
	const base = `${DOWNLOAD_BASE}${encodeURIComponent(tag)}/`;
	if (platform.id === "windows") return renderPowerShell(base, asset, origin, token);
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
SYNC_URL=${shellQuote(origin)} SYNC_TOKEN=${shellQuote(token)} "$install_tmp/tokscale-client" connect
mkdir -p "$HOME/.local/bin"
install -m 755 "$install_tmp/tokscale-client" "$HOME/.local/bin/tokscale-client"
echo 'Installed and connected. Start collecting with:'
echo '  "$HOME/.local/bin/tokscale-client" run'
`;
}

function renderPowerShell(base: string, asset: string, origin: string, token: string): string {
	return `# Tokscale collector installer
$ErrorActionPreference = 'Stop'
if ([Environment]::OSVersion.Platform -ne 'Win32NT' -or $env:PROCESSOR_ARCHITECTURE -eq 'ARM64' -or $env:PROCESSOR_ARCHITEW6432 -eq 'ARM64' -or -not [Environment]::Is64BitOperatingSystem) { throw 'Select the installer for your OS/architecture.' }
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$installTemp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
$previousSyncUrl = $env:SYNC_URL
$previousSyncToken = $env:SYNC_TOKEN
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
  $env:SYNC_URL = ${psQuote(origin)}
  $env:SYNC_TOKEN = ${psQuote(token)}
  & $binary connect
  if ($LASTEXITCODE -ne 0) { throw 'Connection verification failed.' }
  $destination = Join-Path $env:LOCALAPPDATA 'Programs\\tokscale'
  New-Item -ItemType Directory -Force -Path $destination | Out-Null
  Copy-Item $binary (Join-Path $destination 'tokscale-client.exe') -Force
  Write-Host 'Installed and connected. Start collecting with:'
  Write-Host ('  & "' + (Join-Path $destination 'tokscale-client.exe') + '" run')
} finally {
  $env:SYNC_URL = $previousSyncUrl
  $env:SYNC_TOKEN = $previousSyncToken
  if (Test-Path $installTemp) { Remove-Item $installTemp -Recurse -Force }
}
`;
}
