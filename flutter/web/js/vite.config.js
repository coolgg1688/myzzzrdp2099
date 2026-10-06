import { defineConfig } from 'vite';
import path from 'path';

export default defineConfig({
    resolve: {
        alias: {
            // libsodium-wrappers@0.7.16's ESM entry (dist/modules-esm/*.mjs)
            // imports './libsodium.mjs', which the npm tarball no longer ships.
            // Point the browser bundle at the working CJS build instead.
            'libsodium-wrappers': path.resolve(__dirname, 'node_modules/libsodium-wrappers/dist/modules/libsodium-wrappers.js'),
        },
    },
    build: {
        manifest: false,
        rollupOptions: {
            output: {
                entryFileNames: `[name].js`,
                chunkFileNames: `[name].js`,
                assetFileNames: `[name].[ext]`,
            }
        }
    },
})
