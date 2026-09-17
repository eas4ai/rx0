import { defineConfig } from '@rstest/core';

export default defineConfig({
  // Our UI modules touch document/window/localStorage at import and call
  // time, so tests run in a simulated DOM. happy-dom is the lighter pick.
  testEnvironment: 'happy-dom',
});
