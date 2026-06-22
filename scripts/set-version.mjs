import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const version = process.argv[2];
const stableSemver = /^\d+\.\d+\.\d+$/;

if (!version || !stableSemver.test(version)) {
  console.error("Usage: npm run version:set -- X.Y.Z");
  console.error("Example: npm run version:set -- 1.2.0");
  process.exit(1);
}

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function updateJson(relativePath, update) {
  const filePath = path.join(projectRoot, relativePath);
  const json = JSON.parse(fs.readFileSync(filePath, "utf8"));
  update(json);
  fs.writeFileSync(filePath, `${JSON.stringify(json, null, 2)}\n`);
}

function replacePackageVersion(relativePath, format, replacement) {
  const filePath = path.join(projectRoot, relativePath);
  const contents = fs.readFileSync(filePath, "utf8");

  if (!format.test(contents)) {
    throw new Error(`Could not find the application version in ${relativePath}`);
  }

  const updated = contents.replace(format, replacement);
  fs.writeFileSync(filePath, updated);
}

updateJson("package.json", (json) => {
  json.version = version;
});

updateJson("package-lock.json", (json) => {
  json.version = version;
  if (json.packages?.[""]) {
    json.packages[""].version = version;
  }
});

updateJson("src-tauri/tauri.conf.json", (json) => {
  json.version = version;
});

replacePackageVersion(
  "src-tauri/Cargo.toml",
  /(^\[package\][\s\S]*?^version = ")[^"]+(")/m,
  `$1${version}$2`,
);

replacePackageVersion(
  "src-tauri/Cargo.lock",
  /(\[\[package\]\]\r?\nname = "video-downloader"\r?\nversion = ")[^"]+(")/,
  `$1${version}$2`,
);

console.log(`Video Downloader version updated to ${version}:`);
console.log("  package.json");
console.log("  package-lock.json");
console.log("  src-tauri/Cargo.toml");
console.log("  src-tauri/Cargo.lock");
console.log("  src-tauri/tauri.conf.json");
console.log(`\nNext: review the changes, commit them, then tag the release v${version}.`);
