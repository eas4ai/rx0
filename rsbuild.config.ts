import { defineConfig } from '@rsbuild/core';

export default defineConfig({
  source: {
    // Single entry: the whole UI (including xterm.js, now an npm import)
    // bundles into one file served at /static/app.js.
    entry: { index: './web/src/main.js' },
  },
  output: {
    target: 'web',
    // Emit straight into web/ next to the sources; never wipe the dir.
    distPath: { root: 'web', js: '.', css: '.', svg: '.', font: '.', image: '.', media: '.' },
    cleanDistPath: false,
    filename: { js: 'app.js' },
    // No hashed names, no manifests: the Rust server serves fixed paths.
    filenameHash: false,
  },
  performance: {
    // One file, no vendor chunks or async splits.
    chunkSplit: { strategy: 'all-in-one' },
  },
  tools: {
    // No generated HTML: index.html is authored and served as-is.
    htmlPlugin: false,
    rspack: {
      output: { iife: true },
      // No source maps in the shipped bundle.
      devtool: false,
    },
  },
});
