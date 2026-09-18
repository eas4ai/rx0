// scripts/prepare-sidecar.js
// Stage the rx0 server as a Tauri sidecar: build it, then copy it into
// src-tauri/binaries with the target-triple suffix Tauri requires
// (plus .exe on Windows). Plain node (no shell) so `npm run desktop:*`
// works on Windows too. Usage: node scripts/prepare-sidecar.js [debug|release]
import { execFileSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import path from 'node:path';

let profile = process.argv[2] === 'release' ? 'release' : 'debug';
const cargoArgs = profile === 'release' ? ['build', '--release'] : ['build'];
execFileSync('cargo', cargoArgs, { stdio: 'inherit' });

const triple = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
  .split('\n')
  .find((l) => l.startsWith('host: '))
  .slice('host: '.length)
  .trim();
const ext = triple.includes('windows') ? '.exe' : '';
// Plain `cargo build` has no triple subdir; --target builds do.
const targetDir = process.env.CARGO_TARGET_DIR ?? 'target';
const tripled = path.join(targetDir, triple, profile, `rx0${ext}`);
const plain = path.join(targetDir, profile, `rx0${ext}`);
const src = existsSync(tripled) ? tripled : plain;
const destDir = path.join('src-tauri', 'binaries');
mkdirSync(destDir, { recursive: true });
const dest = path.join(destDir, `rx0-sidecar-${triple}${ext}`);
copyFileSync(src, dest);
console.log(`sidecar: ${dest}`);
