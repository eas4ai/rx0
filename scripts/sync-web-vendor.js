// scripts/sync-web-vendor.js
// Sync the xterm.js stylesheet from the npm package into web/vendor,
// where index.html links it. Plain node (no shell cp) so `npm run build`
// works on Windows too. Run via `npm run build`, not directly.
import { copyFileSync } from 'node:fs';

copyFileSync(
  'node_modules/@xterm/xterm/css/xterm.css',
  'web/vendor/xterm-6.0.0.css',
);
console.log('synced web/vendor/xterm-6.0.0.css');
