import { defineConfig, type Plugin } from "vite";
import { createReadStream, existsSync } from "node:fs";
import { resolve } from "node:path";

/**
 * Serve the repo's data/ directory at /data during development, so the browser
 * build reads the same digests the pipeline just wrote instead of the
 * published copy.
 */
function localData(): Plugin {
  const root = resolve(__dirname, "..", "data");
  return {
    name: "nearfield-local-data",
    configureServer(server) {
      server.middlewares.use("/data", (req, res, next) => {
        const name = (req.url ?? "").replace(/^\/+/, "").split("?")[0];
        const file = resolve(root, name);
        if (!name || !file.startsWith(root) || !existsSync(file)) return next();
        res.setHeader("Content-Type", "application/json");
        createReadStream(file).pipe(res);
      });
    },
  };
}

export default defineConfig({
  plugins: [localData()],
  // Relative base so the same bundle works at github.io/nearfield/ and
  // inside the Android app, where assets load from the bundle root.
  base: "./",
  build: { outDir: "dist", emptyOutDir: true, target: "es2022" },
  server: { port: 5173, strictPort: false },
  clearScreen: false,
});
