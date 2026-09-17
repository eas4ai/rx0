import { defineConfig, js } from '@rslint/core';
import globals from 'globals';

export default defineConfig([
  js.configs.recommended,
  {
    languageOptions: { globals: globals.browser },
    rules: {
      // Codebase idiom: empty catch guards storage/DOM access that may
      // throw in private mode or cross-origin contexts.
      'no-empty': ['error', { allowEmptyCatch: true }],
      // Same idiom: caught errors are reported via toast, not the binding.
      // Leading-underscore params document positional call-site intent.
      'no-unused-vars': ['error', { caughtErrors: 'none', argsIgnorePattern: '^_' }],
      // The rsbuild bundle scopes every module: cross-file use without
      // an import is a runtime ReferenceError (the old concatenating
      // bundler used to allow it).
      'no-undef': 'error',
    },
  },
]);
