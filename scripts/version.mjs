import fs from "node:fs";
import path from "node:path";

const [command, requested] = process.argv.slice(2);
const version = requested?.replace(/^v/, "");
const semver = /^\d+\.\d+\.\d+$/;

if (!['set', 'check'].includes(command) || !version || !semver.test(version)) {
  console.error("Usage: node scripts/version.mjs <set|check> <X.Y.Z|vX.Y.Z>");
  process.exit(2);
}

const root = path.resolve(import.meta.dirname, "..");
const jsonFiles = [
  ["package.json", (json) => { json.version = version; }],
  ["package-lock.json", (json) => {
    json.version = version;
    json.packages[""].version = version;
  }],
  ["src-tauri/tauri.conf.json", (json) => {
    json.version = version;
    const main = json.app?.windows?.find((w) => w.label === "main");
    if (main) main.title = `Murmur — v${version}`;
  }],
];

function readJson(relative) {
  return JSON.parse(fs.readFileSync(path.join(root, relative), "utf8"));
}

function packageVersion(relative) {
  const text = fs.readFileSync(path.join(root, relative), "utf8");
  const packageStart = text.indexOf('name = "murmur"');
  const match = text.slice(packageStart).match(/version = "([^"]+)"/);
  if (!match) throw new Error(`Could not find Murmur version in ${relative}`);
  return match[1];
}

function mainWindowTitleVersion(relative) {
  const main = readJson(relative).app?.windows?.find((w) => w.label === "main");
  const match = main?.title?.match(/v(\d+\.\d+\.\d+)/);
  if (!match) throw new Error(`Could not find a versioned main window title in ${relative}`);
  return match[1];
}

const current = {
  "package.json": readJson("package.json").version,
  "package-lock.json": readJson("package-lock.json").version,
  "src-tauri/tauri.conf.json": readJson("src-tauri/tauri.conf.json").version,
  "src-tauri/tauri.conf.json (main window title)": mainWindowTitleVersion("src-tauri/tauri.conf.json"),
  "src-tauri/Cargo.toml": packageVersion("src-tauri/Cargo.toml"),
  "src-tauri/Cargo.lock": packageVersion("src-tauri/Cargo.lock"),
};

if (command === "check") {
  const mismatches = Object.entries(current).filter(([, value]) => value !== version);
  if (mismatches.length) {
    for (const [file, value] of mismatches) {
      console.error(`${file}: expected ${version}, found ${value}`);
    }
    process.exit(1);
  }
  console.log(`All Murmur versions match ${version}.`);
  process.exit(0);
}

for (const [relative, mutate] of jsonFiles) {
  const json = readJson(relative);
  mutate(json);
  fs.writeFileSync(path.join(root, relative), `${JSON.stringify(json, null, 2)}\n`);
}

for (const relative of ["src-tauri/Cargo.toml", "src-tauri/Cargo.lock"]) {
  const absolute = path.join(root, relative);
  const text = fs.readFileSync(absolute, "utf8");
  const packageStart = text.indexOf('name = "murmur"');
  const prefix = text.slice(0, packageStart);
  const packageAndRest = text
    .slice(packageStart)
    .replace(/version = "[^"]+"/, `version = "${version}"`);
  fs.writeFileSync(absolute, prefix + packageAndRest);
}

console.log(`Set Murmur version to ${version}. Review and commit the changed files before tagging.`);
