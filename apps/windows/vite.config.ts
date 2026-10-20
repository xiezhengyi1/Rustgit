/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "chrome105", outDir: "dist", sourcemap: false },
  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["test/setup.ts"],
    globalSetup: ["test/global-teardown.ts"],
    include: ["test/**/*.test.{ts,tsx}"],
    testTimeout: 20000,
  },
});
