import { spawn, spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const frontendRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const appRoot = resolve(frontendRoot, "..");
const repositoryRoot = resolve(appRoot, "..", "..");
const executableSuffix = process.platform === "win32" ? ".exe" : "";

const host = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
if (host.status !== 0) {
  process.stderr.write(host.stderr || "rustc is required to run the desktop app.\n");
  process.exit(host.status ?? 1);
}

const target = host.stdout.match(/^host:\s*(\S+)/m)?.[1];
if (!target) {
  process.stderr.write("Could not determine the Rust host target.\n");
  process.exit(1);
}

const build = spawnSync("cargo", ["build", "--locked", "-p", "focuser-cli"], {
  cwd: repositoryRoot,
  stdio: "inherit",
});
if (build.status !== 0) process.exit(build.status ?? 1);

const binaryDirectory = resolve(appRoot, "binaries");
mkdirSync(binaryDirectory, { recursive: true });
copyFileSync(
  resolve(repositoryRoot, "target", "debug", `focuser-cli${executableSuffix}`),
  resolve(binaryDirectory, `focuser-cli-${target}${executableSuffix}`),
);

const tauri = spawn("cargo", ["tauri", "dev"], {
  cwd: appRoot,
  stdio: "inherit",
});
tauri.on("error", (error) => {
  process.stderr.write(`Could not start Tauri: ${error.message}\n`);
  process.exitCode = 1;
});
tauri.on("exit", (code, signal) => {
  process.exitCode = code ?? (signal ? 1 : 0);
});
