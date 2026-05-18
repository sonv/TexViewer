// Flat-config ESLint setup for the embedded viewer bundles.
//
// This is intentionally minimal — we want the "renamed-a-function, forgot-a-
// callsite" class of bug to fail loud, without committing to a full
// formatter/style policy. The rule set is:
//   * no-undef: catches typo'd identifiers and forgotten renames
//   * no-unused-vars: catches dead code from incomplete refactors
//   * no-unreachable: catches accidental early-return / throw bugs
//   * no-dupe-keys / no-dupe-args: catches copy-paste errors in object literals
//
// Browser, MathJax, and the engine adapter shim are declared as globals.

import js from "@eslint/js";
import globals from "globals";

export default [
  js.configs.recommended,
  {
    files: ["crates/core/src/assets/client.js"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "script",
      globals: {
        ...globals.browser,
        // window.__mpEngine is provided by the per-engine adapter JS that
        // mathpreview concatenates into the page before client.js runs.
        __mpEngine: "readonly",
      },
    },
    rules: {
      "no-undef": "error",
      "no-unused-vars": [
        "warn",
        {
          args: "none",
          varsIgnorePattern: "^_",
          caughtErrors: "none",
        },
      ],
      "no-unreachable": "error",
      "no-dupe-keys": "error",
      "no-dupe-args": "error",
      // Empty catch blocks are an intentional defensive pattern
      // ("swallow this error if MathJax isn't loaded yet").
      "no-empty": ["error", { allowEmptyCatch: true }],
    },
  },
  {
    files: ["crates/core/src/engines/assets/mathjax.js"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "script",
      globals: {
        ...globals.browser,
        MathJax: "readonly",
      },
    },
    rules: {
      "no-undef": "error",
      "no-unused-vars": [
        "warn",
        {
          args: "none",
          varsIgnorePattern: "^_",
          caughtErrors: "none",
        },
      ],
      "no-empty": ["error", { allowEmptyCatch: true }],
    },
  },
];
