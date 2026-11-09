import { defineConfig } from "vite-plus";

export default defineConfig({
  pack: {
    entry: { index: "./sources/repository/src/main.ts" },
    format: "esm",
    outDir: "dist",
    clean: false,
    exports: true,
    dts: {
      generator: "tsgo",
    },
    deps: {
      resolveDepSubpath: true,
    },
  },
  test: {
    include: ["sources/*/src/**/*.{test,spec}.ts", "packages/net/src/**/*.{test,spec}.ts"],
    hookTimeout: 180_000,
    testTimeout: 60_000,
  },
  staged: {
    "*": "vp check --fix",
  },
  fmt: {},
  lint: {
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: { "vite-plus/prefer-vite-plus-imports": "error" },
    options: { typeAware: true, typeCheck: true },
  },
  run: {
    // Cargo manages incremental native builds; preserve Rust toolchain environment variables.
    cache: { scripts: false, tasks: true },
  },
});
